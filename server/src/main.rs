use actix_web::{get, web, App, HttpServer, Responder};

#[get("/repo/{owner}/{repo}/loc")]
async fn loc(path: web::Path<(String, String)>) -> impl Responder {
    let (owner, repo) = path.into_inner();
    format!("{} {}", owner, repo)
}

#[actix_web::main]
async fn main() -> eyre::Result<()> {
    HttpServer::new(|| App::new().service(loc))
        .bind(("127.0.0.1", 8080))?
        .run()
        .await?;

    Ok(())
}
