use http_dir::fs::disk::DiskFilesystem;
use http_dir::ServeDir;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let service = ServeDir::new(DiskFilesystem::from("examples"));

    // Run our service using `hyper`
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 3000));
    hyper::Server::bind(&addr)
        .serve(tower::make::Shared::new(service))
        .await
        .expect("server error");
}
