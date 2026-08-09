pub fn normalize(path: &str) -> Result<String, String> {
    if path.starts_with('/') {
        return Err("absolute path".to_string());
    }
    Ok(path.replace('\\', "/"))
}
