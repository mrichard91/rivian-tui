use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{Local, Utc};
use rumqttc::{AsyncClient, MqttOptions, QoS};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::api::types::{ChargingSession, VehicleStateFields};
use crate::app::{AppEvent, LogEntry, LogLevel};
use crate::config::MqttConfig;

#[derive(Clone)]
pub struct MqttPublisher {
    config: MqttConfig,
    tx: mpsc::UnboundedSender<PublishRequest>,
}

struct PublishRequest {
    topic: String,
    payload: String,
}

#[derive(Serialize)]
struct ObservationEnvelope<'a, T> {
    schema: &'static str,
    source_name: &'a str,
    channel_name: &'a str,
    sensor_name: &'a str,
    vehicle_id: &'a str,
    observation_type: &'static str,
    collected_at: String,
    payload: &'a T,
}

impl MqttPublisher {
    /// Start the MQTT publisher. `event_tx` is the app's event channel; the
    /// publisher uses it to surface broker/publish errors as activity-log
    /// entries instead of writing to stderr, which would otherwise mangle
    /// the ratatui alternate-screen display.
    pub fn start(config: MqttConfig, event_tx: mpsc::UnboundedSender<AppEvent>) -> Result<Self> {
        let mut options = MqttOptions::new(config.client_id(), config.host.clone(), config.port);
        options.set_keep_alive(Duration::from_secs(config.keep_alive_secs.into()));
        if let Some(username) = config.username.clone() {
            options.set_credentials(username, config.password.clone().unwrap_or_default());
        }

        let (client, mut eventloop) = AsyncClient::new(options, config.inflight);
        let (tx, mut rx) = mpsc::unbounded_channel::<PublishRequest>();

        let eventloop_config = config.clone();
        let eventloop_log_tx = event_tx.clone();
        tokio::spawn(async move {
            // An unreachable broker fails poll() immediately, so without
            // backoff this loop would push an error into the activity log
            // about once per second — flooding out all other history within
            // minutes. Back off exponentially and log only the first failure
            // of a streak (with a periodic "still failing" reminder).
            const MAX_BACKOFF_SECS: u64 = 60;
            const REMIND_EVERY: u64 = 50;
            let mut consecutive_failures: u64 = 0;
            loop {
                match eventloop.poll().await {
                    Ok(_) => {
                        if consecutive_failures > 0 {
                            let _ = eventloop_log_tx.send(AppEvent::ServiceLog(LogEntry {
                                timestamp: Local::now(),
                                level: LogLevel::Info,
                                message: format!(
                                    "MQTT reconnected to {} after {consecutive_failures} failed attempt(s)",
                                    eventloop_config.broker_label()
                                ),
                                detail: None,
                            }));
                        }
                        consecutive_failures = 0;
                    }
                    Err(err) => {
                        consecutive_failures += 1;
                        if consecutive_failures == 1
                            || consecutive_failures.is_multiple_of(REMIND_EVERY)
                        {
                            let _ = eventloop_log_tx.send(AppEvent::ServiceLog(LogEntry {
                                timestamp: Local::now(),
                                level: LogLevel::Error,
                                message: format!(
                                    "MQTT event loop error for {} ({} consecutive): {err}",
                                    eventloop_config.broker_label(),
                                    consecutive_failures
                                ),
                                detail: None,
                            }));
                        }
                        let backoff = 1u64
                            .checked_shl(consecutive_failures.min(6) as u32)
                            .unwrap_or(MAX_BACKOFF_SECS)
                            .min(MAX_BACKOFF_SECS);
                        tokio::time::sleep(Duration::from_secs(backoff)).await;
                    }
                }
            }
        });

        let publish_client = client.clone();
        let publish_config = config.clone();
        let publish_log_tx = event_tx;
        tokio::spawn(async move {
            while let Some(request) = rx.recv().await {
                if let Err(err) = publish_client
                    .publish(
                        request.topic,
                        qos_level(&publish_config),
                        publish_config.retain,
                        request.payload,
                    )
                    .await
                {
                    let _ = publish_log_tx.send(AppEvent::ServiceLog(LogEntry {
                        timestamp: Local::now(),
                        level: LogLevel::Error,
                        message: format!(
                            "MQTT publish error for {}: {err}",
                            publish_config.broker_label()
                        ),
                        detail: None,
                    }));
                }
            }
        });

        Ok(Self { config, tx })
    }

    pub fn publish_vehicle_state(
        &self,
        vehicle_id: &str,
        state: &VehicleStateFields,
    ) -> Result<()> {
        if !self.config.publish_vehicle_state {
            return Ok(());
        }

        self.queue_publish(vehicle_id, "vehicle_state", state)
    }

    pub fn publish_charging_session(
        &self,
        vehicle_id: &str,
        session: &ChargingSession,
    ) -> Result<()> {
        if !self.config.publish_charging_sessions {
            return Ok(());
        }

        self.queue_publish(vehicle_id, "charging_session", session)
    }

    fn queue_publish<T: Serialize>(
        &self,
        vehicle_id: &str,
        observation_type: &'static str,
        payload: &T,
    ) -> Result<()> {
        let topic = self.config.observation_topic(vehicle_id, observation_type);
        let sensor_name = self.config.sensor_name_for(vehicle_id);
        let envelope = ObservationEnvelope {
            schema: "rivian_tui.observation.v1",
            source_name: &self.config.source_name,
            channel_name: &self.config.channel_name,
            sensor_name: &sensor_name,
            vehicle_id,
            observation_type,
            collected_at: Utc::now().to_rfc3339(),
            payload,
        };
        let payload =
            serde_json::to_string(&envelope).context("failed to serialize MQTT observation")?;

        self.tx
            .send(PublishRequest { topic, payload })
            .map_err(|_| anyhow!("mqtt publisher queue closed"))
    }
}

fn qos_level(config: &MqttConfig) -> QoS {
    match config.qos {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        _ => QoS::ExactlyOnce,
    }
}
