use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub deepseek_token: String,
    #[serde(default)]
    pub proxyapi_token: String,
    #[serde(default)]
    pub openrouter_token: String,
    #[serde(default = "default_show_system")]
    pub show_system: bool,
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_secs: u64,
    #[serde(default = "default_font_name")]
    pub font_name: String,
    #[serde(default = "default_font_size")]
    pub font_size: i32,
    #[serde(default = "default_font_bold")]
    pub font_bold: bool,
    #[serde(default = "default_outline_width")]
    pub outline_width: u32,
    #[serde(default = "default_text_color")]
    pub text_color: String,
    #[serde(default = "default_outline_color")]
    pub outline_color: String,
    #[serde(default = "default_opacity")]
    pub opacity: u8,
    #[serde(default = "default_pos_x")]
    pub pos_x: i32,
    #[serde(default = "default_pos_y")]
    pub pos_y: i32,
}

fn default_show_system() -> bool { true }
fn default_refresh_interval() -> u64 { 60 }
fn default_font_name() -> String { "Consolas".into() }
fn default_font_size() -> i32 { 18 }
fn default_font_bold() -> bool { true }
fn default_outline_width() -> u32 { 1 }
fn default_text_color() -> String { "FFFFFF".into() }
fn default_outline_color() -> String { "000000".into() }
fn default_opacity() -> u8 { 200 }
fn default_pos_x() -> i32 { 10 }
fn default_pos_y() -> i32 { 10 }

impl Default for Config {
    fn default() -> Self {
        Self {
            deepseek_token: String::new(),
            proxyapi_token: String::new(),
            openrouter_token: String::new(),
            show_system: default_show_system(),
            refresh_interval_secs: default_refresh_interval(),
            font_name: default_font_name(),
            font_size: default_font_size(),
            font_bold: default_font_bold(),
            outline_width: default_outline_width(),
            text_color: default_text_color(),
            outline_color: default_outline_color(),
            opacity: default_opacity(),
            pos_x: default_pos_x(),
            pos_y: default_pos_y(),
        }
    }
}

impl Config {
    pub fn load_or_create(path: &str) -> Self {
        let p = Path::new(path);
        if p.exists() {
            // Never write back over an existing file: a typo in the TOML would
            // otherwise wipe the user's API tokens.
            return match std::fs::read_to_string(p)
                .map_err(|e| e.to_string())
                .and_then(|c| toml::from_str(&c).map_err(|e| e.to_string()))
            {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("Config unusable: {}. Running on defaults, file kept.", e);
                    Config::default()
                }
            };
        }
        let cfg = Config::default();
        if let Err(e) = cfg.save(path) {
            eprintln!("Failed to save default config: {}", e);
        }
        cfg
    }

    pub fn save(&self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}
