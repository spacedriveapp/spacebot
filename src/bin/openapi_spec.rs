fn main() {
    match spacebot::openapi_json() {
        Ok(json) => print!("{}", json),
        Err(e) => {
            eprintln!("Error generating OpenAPI spec: {}", e);
            std::process::exit(1);
        }
    }
}
