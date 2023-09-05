use actix_web::{error, get, web, App, HttpServer, Responder};
use git2::Repository;
use tokei::{Config, Languages};

#[derive(serde::Serialize)]
struct LangStat {
    language: String,
    code: usize,
    comments: usize,
    blanks: usize,
}

#[get("/repo/{owner}/{repo}/loc")]
async fn loc(path: web::Path<(String, String)>) -> actix_web::Result<impl Responder> {
    let (owner, repo) = path.into_inner();

    let url = format!("https://github.com/{}/{}", owner, repo);
    let path = format!("./repos/{}/{}", owner, repo);

    Repository::clone(url.as_str(), &path).map_err(error::ErrorInternalServerError)?;

    let mut languages = Languages::new();

    languages.get_statistics(&[&path], &[], &Config::default());

    std::fs::remove_dir_all(path).map_err(error::ErrorInternalServerError)?;

    Ok(web::Json(
        languages
            .iter()
            .map(|(lang, stat)| LangStat {
                language: lang.to_string(),
                code: stat.code,
                comments: stat.comments,
                blanks: stat.blanks,
            })
            .collect::<Vec<_>>(),
    ))
}

#[actix_web::main]
async fn main() -> eyre::Result<()> {
    HttpServer::new(|| App::new().service(loc))
        .bind(("127.0.0.1", 8080))?
        .run()
        .await?;

    Ok(())
}
