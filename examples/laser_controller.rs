use laser_controller::ControlCommand;
use tokio::runtime::Builder;
use tokio::sync::mpsc;
use tokio::sync::broadcast;
use tokio::time::{Duration, sleep};
use tracing::{info, error};
use tracing_subscriber::{FmtSubscriber, EnvFilter};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    info!("Starting Laser Controller Example");

    // Create channels for communication
    let (control_tx, control_rx) = mpsc::channel(10);
    let (dac_data_tx, mut dac_data_rx) = broadcast::channel(10);
    let (_adc_data_tx, _adc_data_rx) = mpsc::channel::<Vec<f32>>(10);

    // Create runtime
    let runtime = Arc::new(Builder::new_multi_thread()
        .thread_name("laser-controller")
        .thread_stack_size(20 * 1024 * 1024)
        .enable_all()
        .worker_threads(4)
        .build()?);

    // Spawn DAC interface
    let dac_runtime = runtime.clone();
    let dac_task = runtime.spawn(async move {
        if let Err(e) = laser_controller::dac_interface(control_rx, dac_data_tx, dac_runtime).await {
            error!("DAC interface error: {}", e);
        }
    });

    // Send some control commands
    info!("Sending control commands...");
    control_tx.send(ControlCommand::SetAmplitude(2.5)).await?;
    control_tx.send(ControlCommand::SetFrequency(1500.0)).await?;
    control_tx.send(ControlCommand::SetSamplingRate(1_000_000.0)).await?;
    control_tx.send(ControlCommand::Start).await?;

    // Wait for samples to be generated
    info!("Waiting for samples...");
    sleep(Duration::from_millis(100)).await;

    // Check if we received any samples
    let mut sample_count = 0;
    while let Ok(samples) = dac_data_rx.try_recv() {
        sample_count += samples.len();
        info!("Received {} samples", samples.len());
    }

    info!("Total samples received: {}", sample_count);

    // Stop the DAC
    control_tx.send(ControlCommand::Stop).await?;
    info!("DAC stopped");

    // Clean up
    drop(control_tx);
    dac_task.abort();

    info!("Laser Controller Example completed");
    Ok(())
}
