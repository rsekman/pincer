use std::error::Error;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::pincer::Pincer;

/// Struct that interfaces with the Wayland compositor
pub struct Clipboard {
    pincer: Arc<Mutex<Pincer>>,
}

impl Clipboard {
    pub fn new(pincer: Arc<Mutex<Pincer>>) -> Self {
        Clipboard { pincer }
    }

    pub async fn listen(&self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
}
