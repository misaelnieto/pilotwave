//! Queue scope (value object).
//!
//! Queue + backend + allowlist extracted once and reused in every query.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueScope {
    pub queue_name: String,
    pub backend_name: String,
    pub task_paths: Vec<String>,
}

impl QueueScope {
    pub fn new(queue_name: String, backend_name: String, task_paths: Vec<String>) -> Self {
        Self {
            queue_name,
            backend_name,
            task_paths,
        }
    }
}

impl From<&crate::config::Config> for QueueScope {
    fn from(config: &crate::config::Config) -> Self {
        Self::new(
            config.queue_name.clone(),
            config.backend_name.clone(),
            config.task_paths.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_conserva_cola_backend_y_allowlist() {
        let scope = QueueScope::new(
            "pilotwave.email".to_owned(),
            "default".to_owned(),
            vec!["pilotwave.tasks.mail_notification".to_owned()],
        );
        assert_eq!(scope.queue_name, "pilotwave.email");
        assert_eq!(scope.backend_name, "default");
        assert_eq!(scope.task_paths, ["pilotwave.tasks.mail_notification"]);
    }
}
