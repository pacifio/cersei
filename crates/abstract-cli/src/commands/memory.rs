use crate::config::AppConfig;

pub fn run(config: &AppConfig) -> anyhow::Result<()> {
    crate::sessions::show_memory(config)
}
