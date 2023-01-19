use http_dir::fs::include_dir::IncludeDirFilesystem;
use http_dir::ServeDir;
use include_dir::{include_dir, Dir};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    static DIR: Dir<'_> = include_dir!("src");

    let service = ServeDir::new(IncludeDirFilesystem::from(DIR.clone()));

    // Run our service using `hyper`
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 3000));
    hyper::Server::bind(&addr)
        .serve(tower::make::Shared::new(service))
        .await
        .expect("server error");
}
