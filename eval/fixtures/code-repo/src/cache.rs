use std::collections::HashMap;

pub struct PermissionCache {
    values: HashMap<String, Vec<String>>,
}

impl PermissionCache {
    pub fn get(&self, user_id: &str) -> Option<&Vec<String>> {
        self.values.get(user_id)
    }

    pub fn grant(&mut self, user_id: String, permissions: Vec<String>) {
        self.values.insert(user_id, permissions);
    }

    // Intentionally incomplete fixture: revoke must invalidate the cached value.
    pub fn revoke(&mut self, _user_id: &str) {}
}
