use once_cell::sync::OnceCell;
use carbon_core::error::{CarbonResult, Error};
use crate::events::events::SwapResult;
use rabbitmq_stream_client::{
    types::{Message, ByteCapacity, ResponseCode},
    NoDedup,
    Producer,
    error::StreamCreateError,
};
use serde_json;
use tokio::time::{sleep, Duration};

// Global OnceCell to hold the initialized publisher, wrapped in a Box.
static GLOBAL_RABBITMQ_PUBLISHER: OnceCell<Box<RabbitMQPublisher>> = OnceCell::new();


const RABBITMQ_SWAP_EVENTS_TOPIC: &str = "swap_events";
const RABBITMQ_POOL_CREATED_EVENTS_TOPIC: &str = "pool_created_events";
const RABBITMQ_TOKEN_CREATED_EVENTS_TOPIC: &str = "token_created_events";

/// Helper function that attempts to create a producer for the given topic,
/// retrying up to `max_retries` times with a 2‑second delay between attempts.
async fn create_producer_with_retry(
    environment: &rabbitmq_stream_client::Environment,
    topic: &str,
    max_retries: usize,
) -> CarbonResult<Producer<NoDedup>> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        match environment.producer().build(topic).await {
            Ok(prod) => return Ok(prod),
            Err(e) => {
                println!(
                    "Attempt {}: Failed to create producer for topic {}: {}",
                    attempts, topic, e
                );
                if attempts >= max_retries {
                    return Err(Error::Custom(format!(
                        "Failed to create producer for topic {} after {} attempts: {}",
                        topic, attempts, e
                    )));
                }
                // Wait 2 seconds before retrying
                sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

/// Our publisher type that holds a RabbitMQ producer.
pub struct RabbitMQPublisher {
    producer_swap_result: Producer<NoDedup>,
    producer_pool_created: Producer<NoDedup>,
    producer_token_created: Producer<NoDedup>,
}

impl RabbitMQPublisher {
    
    /// Asynchronously creates a new publisher instance and stores it globally.
    pub async fn init(
        rabbitmq_host: String,
        rabbitmq_port: u16,
        rabbitmq_user: String,
        rabbitmq_password: String,
        rabbitmq_vhost: String,
    ) -> CarbonResult<()> {
        println!(
            "Connecting to RabbitMQ at {}:{}/{}",
            rabbitmq_host, rabbitmq_port, rabbitmq_vhost
        );
        use rabbitmq_stream_client::Environment;
        
        // Retry logic for creating the environment
        let max_retries = 500;
        let environment = {
            let mut attempts = 0;
            loop {
                attempts += 1;
                match Environment::builder()
                    .host(&rabbitmq_host)
                    .port(rabbitmq_port)
                    .username(&rabbitmq_user)
                    .password(&rabbitmq_password)
                    .virtual_host(&rabbitmq_vhost)
                    .build()
                    .await
                {
                    Ok(env) => break env,
                    Err(e) => {
                        println!(
                            "Attempt {}: Failed to create environment: {:#?}",
                            attempts, e
                        );
                        if attempts >= max_retries {
                            return Err(Error::Custom(format!(
                                "Failed to create environment after {} attempts: {}",
                                attempts,
                                e
                            )));
                        }
                        // Wait 2 seconds before retrying
                        sleep(Duration::from_secs(2)).await;
                    }
                }
            }
        };

        let topics = [RABBITMQ_SWAP_EVENTS_TOPIC, RABBITMQ_POOL_CREATED_EVENTS_TOPIC, RABBITMQ_TOKEN_CREATED_EVENTS_TOPIC];   
        for topic in topics {
            // Attempt to create the stream (ignoring StreamAlreadyExists)
            let create_response = environment
                .stream_creator()
                .max_length(ByteCapacity::GB(5))
                .create(&topic)
                .await;

            if let Err(e) = create_response {
                if let StreamCreateError::Create { stream, status } = e {
                    match status {
                        // we can ignore this error because the stream already exists
                        ResponseCode::StreamAlreadyExists => {}
                        err => {
                            println!("Error creating stream: {:?} {:?}", stream, err);
                        }
                    }
                }
            }
        }

        // Use Tokio's join! macro to concurrently run the producer creation tasks.
        let (producer_swap_result, producer_pool_created, producer_token_created) = tokio::join!(
            create_producer_with_retry(&environment, RABBITMQ_SWAP_EVENTS_TOPIC, max_retries),
            create_producer_with_retry(&environment, RABBITMQ_POOL_CREATED_EVENTS_TOPIC, max_retries),
            create_producer_with_retry(&environment, RABBITMQ_TOKEN_CREATED_EVENTS_TOPIC, max_retries),
        );
        match (producer_swap_result, producer_pool_created, producer_token_created) {
            (Ok(producer_swap_result), Ok(producer_pool_created), Ok(producer_token_created)) => {
                let publisher = RabbitMQPublisher { producer_swap_result, producer_pool_created, producer_token_created };
                GLOBAL_RABBITMQ_PUBLISHER
                    .set(Box::new(publisher))
                    .map_err(|_| Error::Custom("Global publisher already initialized".to_string()))?;
                Ok(())
            }
            _ => Err(Error::Custom("Failed to create producers".to_string())),
        }
    }

    async fn _publish_swap_result(&self, swap_result: &SwapResult) -> CarbonResult<()> {
        // Serialize the SwapResult to JSON.
        let body = serde_json::to_string(swap_result).unwrap_or_default();
        if !body.is_empty() {
            let message = Message::builder().body(body).build();
            self.producer_swap_result.send(message, |_confirmation_status| async move {
                // println!("Message confirmed with status {:?}", confirmation_status);
            }).await.map_err(|e| {
                println!("Failed to send message: {}", e);
                Error::Custom(e.to_string())
            })?;
        }
        Ok(())
    }
    

    /// Static async method to publish a SwapResult via the global instance.
    /// This allows you to call:
    ///   RabbitMQSwapResultPublisher::publish_swap_result(&swap_result).await;
    pub async fn publish_swap_result(swap_result: &SwapResult) -> CarbonResult<()> {
        if let Some(publisher) = GLOBAL_RABBITMQ_PUBLISHER.get() {
            publisher._publish_swap_result(swap_result).await
        } else {
            Err(Error::Custom("Global publisher not initialized".to_string()))
        }
    }
}
