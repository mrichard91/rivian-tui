use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

const CONFIG_DIR_NAME: &str = "rivian-tui";
const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub mqtt: Option<MqttConfig>,
    #[serde(default)]
    pub web: Option<WebConfig>,
}

impl AppConfig {
    pub fn load(path_override: Option<&Path>) -> Result<Self> {
        let path = match path_override {
            Some(path) => path.to_path_buf(),
            None => Self::default_path()?,
        };

        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to read config: {}", path.display()));
            }
        };

        let config: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse config: {}", path.display()))?;

        if let Some(mqtt) = &config.mqtt {
            mqtt.validate()
                .with_context(|| format!("invalid MQTT config in {}", path.display()))?;
        }

        if let Some(web) = &config.web {
            web.validate()
                .with_context(|| format!("invalid web config in {}", path.display()))?;
        }

        Ok(config)
    }

    pub fn default_path() -> Result<PathBuf> {
        let dir = dirs::config_dir()
            .context("could not determine config directory")?
            .join(CONFIG_DIR_NAME);
        fs::create_dir_all(&dir).context("failed to create config directory")?;
        Ok(dir.join(CONFIG_FILE_NAME))
    }

    pub fn enabled_mqtt(&self) -> Option<&MqttConfig> {
        self.mqtt.as_ref().filter(|config| config.enabled)
    }

    pub fn enabled_web(&self) -> Option<&WebConfig> {
        self.web.as_ref().filter(|config| config.enabled)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_bind")]
    pub bind: String,
    #[serde(default = "default_web_port")]
    pub port: u16,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: default_web_bind(),
            port: default_web_port(),
        }
    }
}

impl WebConfig {
    pub fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if self.bind.trim().is_empty() {
            bail!("web.bind must not be empty when web.enabled = true");
        }
        if self.port == 0 {
            bail!("web.port must be greater than 0");
        }
        Ok(())
    }

    pub fn socket_addr(&self) -> Result<std::net::SocketAddr> {
        format!("{}:{}", self.bind, self.port)
            .parse()
            .with_context(|| format!("invalid web bind address: {}:{}", self.bind, self.port))
    }
}

fn default_web_bind() -> String {
    "0.0.0.0".into()
}

fn default_web_port() -> u16 {
    8787
}

#[derive(Debug, Clone, Deserialize)]
pub struct MqttConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_mqtt_port")]
    pub port: u16,
    pub client_id: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default = "default_topic_prefix")]
    pub topic_prefix: String,
    #[serde(default = "default_channel_name")]
    pub channel_name: String,
    pub sensor_name: Option<String>,
    #[serde(default = "default_source_name")]
    pub source_name: String,
    #[serde(default)]
    pub retain: bool,
    #[serde(default)]
    pub qos: u8,
    #[serde(default = "default_keep_alive_secs")]
    pub keep_alive_secs: u16,
    #[serde(default = "default_inflight")]
    pub inflight: usize,
    #[serde(default = "default_publish_observations")]
    pub publish_vehicle_state: bool,
    #[serde(default = "default_publish_observations")]
    pub publish_charging_sessions: bool,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: default_mqtt_port(),
            client_id: None,
            username: None,
            password: None,
            topic_prefix: default_topic_prefix(),
            channel_name: default_channel_name(),
            sensor_name: None,
            source_name: default_source_name(),
            retain: false,
            qos: 0,
            keep_alive_secs: default_keep_alive_secs(),
            inflight: default_inflight(),
            publish_vehicle_state: true,
            publish_charging_sessions: true,
        }
    }
}

impl MqttConfig {
    pub fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        if self.host.trim().is_empty() {
            bail!("mqtt.host is required when mqtt.enabled = true");
        }

        if self.qos > 2 {
            bail!("mqtt.qos must be 0, 1, or 2");
        }

        if self.inflight == 0 {
            bail!("mqtt.inflight must be at least 1");
        }

        Ok(())
    }

    pub fn client_id(&self) -> String {
        self.client_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| format!("rivian-tui-{}", uuid::Uuid::new_v4()))
    }

    pub fn broker_label(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn sensor_name_for(&self, vehicle_id: &str) -> String {
        self.sensor_name
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| vehicle_id.to_string())
    }

    pub fn observation_topic(&self, vehicle_id: &str, observation_type: &str) -> String {
        let prefix = self.topic_prefix.trim_matches('/');
        let channel = sanitize_topic_segment(&self.channel_name);
        let source = sanitize_topic_segment(&self.source_name);
        let sensor = sanitize_topic_segment(&self.sensor_name_for(vehicle_id));
        let kind = sanitize_topic_segment(observation_type);

        if prefix.is_empty() {
            format!("{channel}/{source}/{sensor}/{kind}")
        } else {
            format!("{prefix}/{channel}/{source}/{sensor}/{kind}")
        }
    }
}

fn default_mqtt_port() -> u16 {
    1883
}
fn default_topic_prefix() -> String {
    "rivian".into()
}
fn default_channel_name() -> String {
    "observations".into()
}
fn default_source_name() -> String {
    "rivian-tui".into()
}
fn default_keep_alive_secs() -> u16 {
    30
}
fn default_inflight() -> usize {
    16
}
fn default_publish_observations() -> bool {
    true
}

fn sanitize_topic_segment(value: &str) -> String {
    let sanitized: String = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.is_empty() {
        "unknown".into()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_returns_defaults() {
        let path =
            std::env::temp_dir().join(format!("rivian-tui-missing-{}.toml", uuid::Uuid::new_v4()));
        let config = AppConfig::load(Some(&path)).unwrap();
        assert!(config.enabled_mqtt().is_none());
    }

    #[test]
    fn web_config_validates_bind_and_port() {
        let good = WebConfig {
            enabled: true,
            bind: "0.0.0.0".into(),
            port: 8787,
        };
        assert!(good.validate().is_ok());

        let bad_port = WebConfig {
            enabled: true,
            bind: "0.0.0.0".into(),
            port: 0,
        };
        assert!(bad_port.validate().is_err());

        let disabled = WebConfig {
            enabled: false,
            bind: String::new(),
            port: 0,
        };
        assert!(disabled.validate().is_ok());
    }

    #[test]
    fn mqtt_topic_uses_custom_sensor_name() {
        let config = MqttConfig {
            enabled: true,
            host: "broker".into(),
            sensor_name: Some("garage truck".into()),
            ..MqttConfig::default()
        };

        assert_eq!(
            config.observation_topic("vehicle-123", "vehicle_state"),
            "rivian/observations/rivian-tui/garage_truck/vehicle_state"
        );
    }
}
