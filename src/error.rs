pub type Error = String;
pub type Anyhow = anyhow::Error;

macro_rules! log_and_pass_on {
    ($msg:expr) => {
        |e| {
            error!("{}: {e}", $msg);
            e
        }
    };
}

pub(crate) use log_and_pass_on;
