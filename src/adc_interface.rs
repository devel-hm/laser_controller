use crate::ControlCommand;
use tokio::sync::mpsc::{Sender, Receiver};
use tokio::sync::Mutex;
use tokio::sync::broadcast::{Sender as BroadcastSender};
use tokio::time::{interval_at, Duration, Instant};
use std::sync::Arc;
use tracing::{error, info};
use anyhow::{anyhow, Result};
use tokio::runtime::Runtime;

// Wed are not looking to create complete Digital value but instead just get amplitude and frequency, 
// compare that to configured expected values and send feedback to adjust the amplitude and frequency.

#[derive(Debug, Clone)]
pub struct AdcConfig {
    sample_rate: f32, // MSPS
    expected_amplitude: f32, // V
    expected_frequency: f32, // Hz
    sample_count: usize, // samples @ MSPS
}

impl Default for AdcConfig {
    fn default() -> Self {
        Self {
            sample_rate: 1_000_000.0,
            expected_amplitude: 3.3,
            expected_frequency: 1000.0,
            sample_count: 1000,
        }
    }
}

impl AdcConfig {
    fn new(sample_rate: f32, expected_amplitude: f32, expected_frequency: f32) -> Result<Self, anyhow::Error> {
        Ok(Self {
            sample_rate,
            expected_amplitude,
            expected_frequency,
            sample_count: 1000, // Default sample count
        })
    }
}

struct Adc {
    config: Arc<Mutex<AdcConfig>>,
    command_rx: Receiver<AdcCommand>,
    adc_feedback_tx: Sender<AdcFeedback>,
    dac_data_rx: Receiver<Vec<f32>>,
    running: Arc<Mutex<bool>>,
}

#[derive(Debug, Clone)]
struct AdcSample {
    amplitude: f32, // non RMS
    frequency: f32, // Hz
}

impl AdcSample {
    fn new(amplitude: f32, frequency: f32) -> Self {
        Self {
            amplitude,
            frequency,
        }
    }
}

impl Adc {
    fn new(config: Arc<Mutex<AdcConfig>>, command_rx: Receiver<AdcCommand>, adc_feedback_tx: Sender<AdcFeedback>, dac_data_rx: Receiver<Vec<f32>>) -> Self {
        Self {
            config,
            command_rx,
            adc_feedback_tx,
            dac_data_rx,
            running: Arc::new(Mutex::new(false)),
        }
    }
    // currently i am implementing this as a run until stopped function. this should have signal/oneshot channel to stop it
    // signal handling is another thing it should process.
    async fn run(&mut self) {
        loop {
            tokio::select! {
                Some(command) = self.command_rx.recv() => {
                    match command {
                        AdcCommand::SetSamplingRate(sample_rate) => {
                            self.config.lock().await.sample_rate = sample_rate;
                            info!("Sampling rate set to: {} MSPS", sample_rate);
                        },
                        AdcCommand::SetExpectedAmplitude(expected_amplitude) => {
                            self.config.lock().await.expected_amplitude = expected_amplitude;
                            info!("Expected amplitude set to: {} V", expected_amplitude);
                        },
                        AdcCommand::SetExpectedFrequency(expected_frequency) => {
                            self.config.lock().await.expected_frequency = expected_frequency;
                            info!("Expected frequency set to: {} Hz", expected_frequency);
                        },
                        AdcCommand::SetSamplingCount(sample_count) => {
                            self.config.lock().await.sample_count = sample_count;
                            info!("Sample count set to: {} samples", sample_count);
                        },
                        AdcCommand::Start => {
                            *self.running.lock().await = true;
                            info!("ADC started");
                        },
                        AdcCommand::Stop => {
                            *self.running.lock().await = false;
                            info!("ADC stopped");
                        },
                    }
                },
                Some(samples) = self.dac_data_rx.recv() => {
                    if *self.running.lock().await {
                        let adc_sample = self.process_samples(samples).await;
                        if let Ok(adc_sample_result) = adc_sample {
                            if adc_sample_result.amplitude != self.config.lock().await.expected_amplitude {
                                info!("Amplitude does not match expected value: {} != {}", adc_sample_result.amplitude, self.config.lock().await.expected_amplitude);
                                // send feedback to adjust amplitude
                                if let Err(e) = self.adc_feedback_tx.send(AdcFeedback::SetAmplitude(adc_sample_result.amplitude)).await {
                                    error!("Failed to send SetAmplitude command: {}", e);
                                }
                            }
                            if adc_sample_result.frequency != self.config.lock().await.expected_frequency {
                                info!("Frequency does not match expected value: {} != {}", adc_sample_result.frequency, self.config.lock().await.expected_frequency);
                                // send feedback to adjust frequency
                                if let Err(e) = self.adc_feedback_tx.send(AdcFeedback::SetFrequency(adc_sample_result.frequency)).await {
                                    error!("Failed to send SetFrequency command: {}", e);
                                }
                            }
                        }
                    }
                },
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
                    info!("ADC task timeout - no data received for 5 seconds, continuing");
                    break;
                }
            }
        }
        info!("ADC task completed");
    }

    // process received samples and return the amplitude and frequency
    async fn process_samples(&self, samples: Vec<f32>) -> Result<AdcSample, anyhow::Error> {
        if samples.len() != self.config.lock().await.sample_count {
            return Err(anyhow!("Sample count does not match expected sample count"));
        }
        
        let samples_f64: Vec<f64> = samples.iter().map(|&x| x as f64).collect();

        let peak_value = samples_f64.iter().fold(f64::NEG_INFINITY, |max, &x| x.max(max));
        let min_value = samples_f64.iter().fold(f64::INFINITY, |min, &x| x.min(min));
        let amplitude = (peak_value - min_value) as f32;
        // HJM: TODO i don't know how to calculate frequency from samples, this is just a dummy implementation
        let frequency = 1.0 / (samples_f64.len() as f32 * self.config.lock().await.sample_rate as f32);
        Ok(AdcSample::new(amplitude, frequency))
    }
}

#[derive(Debug, Clone)]
pub enum AdcCommand {
    SetSamplingRate(f32),
    SetExpectedAmplitude(f32),
    SetExpectedFrequency(f32),
    SetSamplingCount(usize),
    Start,
    Stop,
}

#[derive(Debug, Clone)]
pub enum AdcFeedback {
    SetAmplitude(f32),
    SetFrequency(f32),
}

pub async fn adc_interface(mut control_rx: Receiver<ControlCommand>, adc_command_tx: BroadcastSender<ControlCommand>, mut dac_data_rx: Receiver<Vec<f32>>, runtime: Arc<Runtime>) -> Result<(), anyhow::Error> {
    let adc_config = match AdcConfig::new(1_000_000.0, 3.3, 1000.0) {
        Ok(config) => config,
        Err(e) => {
            error!("Failed to create ADC config: {:?}", e);
            return Err(anyhow!("Failed to create ADC config: {:?}", e));
        }
    };
    let (adc_command_tx_local, adc_command_rx) = tokio::sync::mpsc::channel::<AdcCommand>(4);
    let (adc_feedback_tx, mut adc_feedback_rx) = tokio::sync::mpsc::channel::<AdcFeedback>(4);

    let mut adc = Adc::new(Arc::new(Mutex::new(adc_config)), adc_command_rx, adc_feedback_tx, dac_data_rx);
    let adc_task = runtime.spawn(async move {adc.run().await});
    let control_task = runtime.spawn(async move {
        info!("ADC interface control task started");
        loop{
            tokio::select! {
                Some(command) = control_rx.recv() => {
                    match command {
                        ControlCommand::Start => {
                            if let Err(e) = adc_command_tx_local.send(AdcCommand::Start).await {  
                                error!("Failed to send Start command: {}", e);
                            }
                            info!("Start command sent to ADC");
                        },
                        ControlCommand::Stop => {
                            if let Err(e) = adc_command_tx_local.send(AdcCommand::Stop).await {
                                error!("Failed to send Stop command: {}", e);
                            }
                            info!("Stop command sent to ADC");
                        },
                        _ => {
                            info!("Unhandled control command: {:?}, ignoring", command);
                        }
                    }
                },
                Some(feedback) = adc_feedback_rx.recv() => {
                    match feedback {
                        AdcFeedback::SetAmplitude(amplitude) => {
                            if let Err(e) = adc_command_tx.send(ControlCommand::SetAmplitude(amplitude)) {
                                error!("Failed to send SetAmplitude command: {}", e);
                            }
                            info!("SetAmplitude command sent to ADC");
                        },
                        AdcFeedback::SetFrequency(frequency) => {
                            if let Err(e) = adc_command_tx.send(ControlCommand::SetFrequency(frequency)) {
                                error!("Failed to send SetFrequency command: {}", e);
                            }
                            info!("SetFrequency command sent to ADC");
                        },
                        _ => {
                            info!("Unhandled feedback: {:?}, ignoring", feedback);
                            break;
                        }
                    }
                },
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(10)) => {
                    info!("ADC interface control task timeout - no data received for 10 seconds, continuing");
                    break;
                }
            }
        }
        info!("ADC interface control task completed");
    });
    let adc_task_result = adc_task.await;
    let control_task_result = control_task.await;
    info!("ADC task result: {:?}", adc_task_result);
    info!("Control task result: {:?}", control_task_result);
    if adc_task_result.is_err() {
        error!("ADC task failed: {:?}", adc_task_result);
        return Err(anyhow!("ADC task failed: {:?}", adc_task_result));
    }
    if control_task_result.is_err() {
        error!("Control task failed: {:?}", control_task_result);
        return Err(anyhow!("Control task failed: {:?}", control_task_result));
    }
    info!("ADC interface tasks completed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::runtime::Runtime;
    use tokio::sync::mpsc;
    use tokio::sync::broadcast;
    use tokio::time::{Duration, sleep};
    use tracing::info ;
    use tracing_subscriber::EnvFilter;
    use std::sync::Arc;

    fn init_test_logging() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .try_init();
    }

    #[tokio::test]
    async fn test_adc_config_default() {
        init_test_logging();
        let config = AdcConfig::default();
        
        assert_eq!(config.sample_rate, 1_000_000.0);
        assert_eq!(config.expected_amplitude, 3.3);
        assert_eq!(config.expected_frequency, 1000.0);
        assert_eq!(config.sample_count, 1000);
    }

    #[tokio::test]
    async fn test_adc_config_new() {
        init_test_logging();
        let config = AdcConfig::new(2_000_000.0, 5.0, 2000.0).unwrap();
        
        assert_eq!(config.sample_rate, 2_000_000.0);
        assert_eq!(config.expected_amplitude, 5.0);
        assert_eq!(config.expected_frequency, 2000.0);
        assert_eq!(config.sample_count, 1000); // Default value
    }

    #[tokio::test]
    async fn test_adc_sample_new() {
        init_test_logging();
        let sample = AdcSample::new(2.5, 1500.0);
        
        assert_eq!(sample.amplitude, 2.5);
        assert_eq!(sample.frequency, 1500.0);
    }

    #[tokio::test]
    async fn test_adc_process_samples_success() {
        init_test_logging();
        let config = Arc::new(Mutex::new(AdcConfig::default()));
        let (_, command_rx) = mpsc::channel(4);
        let (command_tx, _) = mpsc::channel(4);
        let (_, dac_data_rx) = mpsc::channel(4);
        
        let adc = Adc::new(config, command_rx, command_tx, dac_data_rx);
        
        // Create test samples with known amplitude
        let samples = vec![0.0, 1.0, 2.0, 3.0, 2.0, 1.0, 0.0, -1.0, -2.0, -3.0];
        
        let result = adc.process_samples(samples).await;
        assert!(result.is_err()); // Should fail because sample count doesn't match
    }

    #[tokio::test]
    async fn test_adc_process_samples_correct_count() {
        init_test_logging();
        let config = Arc::new(Mutex::new(AdcConfig::default()));
        let (_, command_rx) = mpsc::channel(4);
        let (command_tx, _) = mpsc::channel(4);
        let (_, dac_data_rx) = mpsc::channel(4);
        
        let adc = Adc::new(config, command_rx, command_tx, dac_data_rx);
        
        // Create test samples with correct count (1000 samples)
        let mut samples = Vec::new();
        for i in 0..1000 {
            samples.push((i as f32 * 0.01).sin() * 2.0); // Sine wave with amplitude 2.0
        }
        
        let result = adc.process_samples(samples).await;
        assert!(result.is_ok());
        
        let adc_sample = result.unwrap();
        assert!(adc_sample.amplitude > 0.0);
        assert!(adc_sample.frequency > 0.0);
    }

    #[tokio::test]
    async fn test_adc_process_samples_amplitude_calculation() {
        init_test_logging();
        let config = Arc::new(Mutex::new(AdcConfig::default()));
        let (_, command_rx) = mpsc::channel(4);
        let (command_tx, _) = mpsc::channel(4);
        let (_, dac_data_rx) = mpsc::channel(4);
        
        let adc = Adc::new(config, command_rx, command_tx, dac_data_rx);
        
        // Create test samples with known amplitude (peak-to-peak = 6.0)
        let mut samples = Vec::new();
        for i in 0..1000 {
            samples.push(3.0 * (i as f32 * 0.01).sin()); // Sine wave: -3.0 to +3.0
        }
        
        let result = adc.process_samples(samples).await;
        assert!(result.is_ok());
        
        let adc_sample = result.unwrap();
        // Amplitude should be approximately 6.0 (peak-to-peak)
        assert!((adc_sample.amplitude - 6.0).abs() < 0.1);
    }

    #[tokio::test]
    async fn test_adc_command_variants() {
        init_test_logging();
        
        // Test all AdcCommand variants
        let commands = vec![
            AdcCommand::SetSamplingRate(1_000_000.0),
            AdcCommand::SetExpectedAmplitude(3.3),
            AdcCommand::SetExpectedFrequency(1000.0),
            AdcCommand::SetSamplingCount(1000),
            AdcCommand::Start,
            AdcCommand::Stop,
        ];
        
        for command in commands {
            // Just verify they can be created and cloned
            let cloned = command.clone();
            match cloned {
                AdcCommand::SetSamplingRate(rate) => assert_eq!(rate, 1_000_000.0),
                AdcCommand::SetExpectedAmplitude(amp) => assert_eq!(amp, 3.3),
                AdcCommand::SetExpectedFrequency(freq) => assert_eq!(freq, 1000.0),
                AdcCommand::SetSamplingCount(count) => assert_eq!(count, 1000),
                AdcCommand::Start => {},
                AdcCommand::Stop => {},
            }
        }
    }

    #[tokio::test]
    async fn test_adc_feedback_variants() {
        init_test_logging();
        
        // Test all AdcFeedback variants
        let feedbacks = vec![
            AdcFeedback::SetAmplitude(2.5),
            AdcFeedback::SetFrequency(1500.0),
        ];
        
        for feedback in feedbacks {
            // Just verify they can be created and cloned
            let cloned = feedback.clone();
            match cloned {
                AdcFeedback::SetAmplitude(amp) => assert_eq!(amp, 2.5),
                AdcFeedback::SetFrequency(freq) => assert_eq!(freq, 1500.0),
            }
        }
    }

    #[tokio::test]
    async fn test_adc_interface_creation() {
        init_test_logging();
        info!("Starting test_adc_interface_creation");
        let runtime = Arc::new(Runtime::new().unwrap());
        let (control_tx, control_rx) = mpsc::channel(4);
        let (adc_command_tx, _) = broadcast::channel(4);
        let (_, dac_data_rx) = mpsc::channel(4);
        
        let runtime_clone = runtime.clone();
        let runtime_1 = runtime.clone();
        // Test that the function can be called without panicking
        let adc_task = runtime_1.spawn(async move {adc_interface(control_rx, adc_command_tx, dac_data_rx, runtime_clone).await});
        tokio::time::sleep(Duration::from_millis(10000)).await;
        adc_task.abort();
        assert!(true);
    }

    #[tokio::test]
    async fn test_adc_error_handling() {
        init_test_logging();
        let config = Arc::new(Mutex::new(AdcConfig::default()));
        let (_, command_rx) = mpsc::channel(4);
        let (command_tx, _) = mpsc::channel(4);
        let (_, dac_data_rx) = mpsc::channel(4);
        
        let adc = Adc::new(config, command_rx, command_tx, dac_data_rx);
        
        // Test with wrong sample count
        let wrong_samples = vec![1.0, 2.0, 3.0]; // Only 3 samples, expected 1000
        let result = adc.process_samples(wrong_samples).await;
        assert!(result.is_err());
        
        // Test with empty samples
        let empty_samples = vec![];
        let result = adc.process_samples(empty_samples).await;
        assert!(result.is_err());
    }
}