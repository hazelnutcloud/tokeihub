use actix_web::http::header;
use actix_web::web::Redirect;
use actix_web::{error, get, web, App, HttpRequest, HttpServer, Responder};
use git2::Repository;
use rand::distributions;
use rand::prelude::*;
use reqwest::StatusCode;
use sea_orm::*;
use sea_orm_migration::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use tokei::{Config, LanguageType, Languages};
use tokeihub::{
    entities::{login_state, prelude::*},
    migrator::Migrator,
};

#[derive(serde::Serialize)]
struct LangStat {
    code: usize,
    comments: usize,
    blanks: usize,
}

/*
Get the lines of code for a github repo
1. Users call this endpoint with a github repo url
2. users pass in their user access token in the header (if they've went through the oauth flow)
3. server checks if user has access to repo, if not, return 401
4. server checks if repo info is already in db, if so, return that
5. server clones repo to local filesystem
6. server runs tokei on repo
7. server deletes repo from local filesystem
8. server stores repo info in db
9. server responds with json containing lines of code for each language
*/
#[get("/v1/repos/{owner}/{repo}/loc")]
async fn loc(
    path: web::Path<(String, String)>,
    req: HttpRequest,
) -> actix_web::Result<impl Responder> {
    let token = req.headers().get(header::AUTHORIZATION);
    let (owner, repo) = path.into_inner();

    let client = reqwest::Client::new();
    let repo_url = format!("https://api.github.com/repos/{}/{}", owner, repo);

    let mut req = client
        .get(repo_url)
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-Github-Api-Version", "2022-11-28")
        .header("User-Agent", "tokeihub");
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, token);
    }

    let res = req.send().await.map_err(error::ErrorInternalServerError)?;
    let status = res.status();

    match status {
        StatusCode::NOT_FOUND => return Err(error::ErrorNotFound("Repo not found")),
        StatusCode::UNAUTHORIZED => return Err(error::ErrorUnauthorized("Unauthorized")),
        StatusCode::FORBIDDEN => return Err(error::ErrorForbidden("Forbidden")),
        _ => {}
    };

    let clone_url = if let Some(token) = token {
        let token = token
            .to_str()
            .map_err(error::ErrorInternalServerError)?
            .split(" ")
            .skip(1)
            .next();
        if let Some(token) = token {
            format!("https://{}@github.com/{}/{}", token, owner, repo)
        } else {
            return Err(error::ErrorUnauthorized("Unauthorized"));
        }
    } else {
        format!("https://github.com/{}/{}", owner, repo)
    };

    let path = format!("./repos/{}/{}", owner, repo);

    Repository::clone(clone_url.as_str(), &path).map_err(error::ErrorInternalServerError)?;

    let mut languages = Languages::new();

    languages.get_statistics(&[&path], &[], &Config::default());

    std::fs::remove_dir_all(path).map_err(error::ErrorInternalServerError)?;

    Ok(web::Json(
        languages
            .into_iter()
            .map(|(lang, stat)| {
                (
                    lang,
                    LangStat {
                        code: stat.code,
                        comments: stat.comments,
                        blanks: stat.blanks,
                    },
                )
            })
            .collect::<HashMap<LanguageType, LangStat>>(),
    ))
}

/*
Main entrypoint
1. Users call this endpoint
2. server generates a random string and stores it in db as state
3. server redirects user to github oauth page with the random string as state
*/
#[get("/v1/auth/login")]
async fn login(data: web::Data<AppState>) -> actix_web::Result<impl Responder> {
    let db = &data.db;

    let state: String = rand::thread_rng()
        .sample_iter(distributions::Alphanumeric)
        .take(10)
        .map(char::from)
        .collect();

    let login_state = login_state::ActiveModel {
        state: ActiveValue::Set(state.clone()),
        ..Default::default()
    };
    LoginState::insert(login_state)
        .exec(db)
        .await
        .map_err(error::ErrorInternalServerError)?;

    Ok(Redirect::to(format!(
        "https://github.com/apps/tokeihub/installations/select_target?state={}",
        state
    )))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: String,
    state: String,
}

/*
Oauth callback
1. Github redirects user here
2. server checks if state is valid
3. server exchanges code for access token
4. server uses access token to get user info
5. server stores user info in db along with access token
6. server responds with json containing access token and user info
7. server deletes state from db
8. client stores access token and user info in local storage
*/
#[get("/v1/auth/callback")]
async fn callback(
    query: web::Query<CallbackQuery>,
    data: web::Data<AppState>,
) -> actix_web::Result<impl Responder> {
    let state = &query.state;
    let code = &query.code;

    let db = &data.db;
    let client_id = &data.client_id;
    let client_secret = &data.client_secret;

    LoginState::delete_many()
        .filter(login_state::Column::State.eq(state))
        .exec(db)
        .await
        .map_err(|e| match e {
            DbErr::RecordNotFound(_) => error::ErrorBadRequest("Invalid State"),
            _ => error::ErrorInternalServerError(e),
        })?;

    let client = reqwest::Client::new();
    let url = format!("https://github.com/login/oauth/access_token?client_id={client_id}&client_secret={client_secret}&code={code}");

    let res = client
        .post(url)
        .send()
        .await
        .map_err(error::ErrorInternalServerError)?;

    if res.status() != StatusCode::OK {
        return Err(error::ErrorBadRequest(res.status()));
    }

    let body = res.text().await.map_err(error::ErrorInternalServerError)?;

    let access_token_info = body
        .split("&")
        .map(|s| {
            let mut split = s.split("=");
            match (split.next(), split.next()) {
                (Some(key), Some(value)) => Ok((key.to_string(), value.to_string())),
                _ => Err(error::ErrorInternalServerError(
                    "Invalid response from github",
                )),
            }
        })
        .collect::<Result<HashMap<String, String>, actix_web::Error>>()?;

    if let Some(error) = access_token_info.get("error") {
        Err(error::ErrorBadRequest(error.to_owned()))
    } else {
        Ok(web::Json(access_token_info))
    }
}

#[derive(Debug, Clone)]
struct AppState {
    db: DatabaseConnection,
    client_id: String,
    client_secret: String,
}

#[actix_web::main]
async fn main() -> eyre::Result<()> {
    dotenvy::dotenv().expect(".env file not found");

    let db_url = env::var("DB_URL")?;
    let client_id = env::var("GH_CLIENT_ID")?;
    let client_secret = env::var("GH_CLIENT_SECRET")?;

    let db = Database::connect(db_url).await?;
    db.execute_unprepared("PRAGMA journal_mode=WAL;").await?;
    Migrator::refresh(&db).await?;

    let state = AppState {
        db,
        client_id,
        client_secret,
    };

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .service(loc)
            .service(login)
            .service(callback)
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await?;

    Ok(())
}
