//! Production Kafka producer using TLS-enabled librdkafka.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use rdkafka::config::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord};
use rdkafka::util::Timeout;

use crate::config::EnrollmentRuntimeConfig;
use crate::domain::events::{KafkaProducer, KafkaPublishError};

pub struct RdkafkaKafkaProducer {
    producer: FutureProducer,
}

impl RdkafkaKafkaProducer {
    pub fn new(config: &EnrollmentRuntimeConfig) -> Result<Self, KafkaPublishError> {
        let mut client = ClientConfig::new();
        client
            .set("bootstrap.servers", &config.bootstrap_servers)
            .set("security.protocol", &config.security_protocol)
            .set("enable.idempotence", "true")
            .set("acks", "all")
            .set("message.timeout.ms", "10000")
            .set("request.timeout.ms", "5000")
            .set("enable.ssl.certificate.verification", "true")
            .set("ssl.endpoint.identification.algorithm", "https");

        if let Some(ca) = &config.ssl_ca_location {
            client.set("ssl.ca.location", ca);
        }
        if let Some(certificate) = &config.ssl_certificate_location {
            client.set("ssl.certificate.location", certificate);
        }
        if let Some(key) = &config.ssl_key_location {
            client.set("ssl.key.location", key);
        }
        if let Some(password) = &config.ssl_key_password {
            client.set("ssl.key.password", password.expose());
        }
        if let Some(mechanism) = &config.sasl_mechanism {
            client.set("sasl.mechanisms", mechanism);
        }
        if let Some(username) = &config.sasl_username {
            client.set("sasl.username", username.expose());
        }
        if let Some(password) = &config.sasl_password {
            client.set("sasl.password", password.expose());
        }

        let producer = client
            .create::<FutureProducer>()
            .map_err(|_| KafkaPublishError::Unavailable)?;
        Ok(Self { producer })
    }
}

impl KafkaProducer for RdkafkaKafkaProducer {
    fn publish<'a>(
        &'a self,
        topic: &'a str,
        key: &'a str,
        payload_json: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), KafkaPublishError>> + Send + 'a>> {
        Box::pin(async move {
            let record = FutureRecord::<str, str>::to(topic)
                .key(key)
                .payload(payload_json);
            // FutureProducer resolves only after broker delivery acknowledgement or failure.
            self.producer
                .send(record, Timeout::After(Duration::from_secs(10)))
                .await
                .map(|_| ())
                .map_err(|_| KafkaPublishError::Unavailable)
        })
    }
}
