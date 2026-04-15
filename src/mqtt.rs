use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rumqttc::{AsyncClient, MqttOptions, QoS};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::api::types::{ChargingSession, VehicleStateFields};
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
    pub fn start(config: MqttConfig) -> Result<Self> {
        let mut options = MqttOptions::new(config.client_id(), config.host.clone(), config.port);
        options.set_keep_alive(Duration::from_secs(config.keep_alive_secs.into()));
        if let Some(username) = config.username.clone() {
            options.set_credentials(username, config.password.clone().unwrap_or_default());
        }

        let (client, mut eventloop) = AsyncClient::new(options, config.inflight);
        let (tx, mut rx) = mpsc::unbounded_channel::<PublishRequest>();

        let eventloop_config = config.clone();
        tokio::spawn(async move {
            loop {
                if let Err(err) = eventloop.poll().await {
                    eprintln!(
                        "MQTT event loop error for {}: {err}",
                        eventloop_config.broker_label()
                    );
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        });

        let publish_client = client.clone();
        let publish_config = config.clone();
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
                    eprintln!(
                        "MQTT publish error for {}: {err}",
                        publish_config.broker_label()
                    );
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
