use crate::ControlCommand;
use tokio::sync::mpsc::Receiver;
use tokio::sync::Mutex;
use tokio::runtime::Runtime;
use std::sync::Arc;
use tracing::{warn, error, info};
use tokio::sync::broadcast::Sender as BroadcastSender;
use anyhow::{anyhow, Result};
use tokio::time::{interval_at, Duration, Instant};

#[derive(Debug, Clone)]
pub struct DacConfig {
    amplitude: f32, // max amplitude
    frequency: f32, // Hz
    sample_rate: f32, //MSPS
    sample_count: usize, // samples @ MSPS
    phase_offset: f32, // radians
    sample_array_default: Option<Vec<f32>>,
}

impl Default for DacConfig {
    fn default() -> Self {
        Self {
            amplitude: 3.3,
            frequency: 1000.0,
            sample_array_default: None,
            phase_offset: 0.0,
            sample_rate: 1_000_000.0,
            sample_count: 1000,
        }
    }
}

impl DacConfig {
    fn new(amplitude: f32, frequency: f32, sample_rate: f32, sample_count: usize, phase_offset: f32, sample_array_default: Option<Vec<f32>>) -> Result<Self, anyhow::Error> {
        if sample_count < 1 {
            error!("sample_count must be greater than 0");
            return Err(anyhow::anyhow!("sample_count must be greater than 0"));
        }
        if sample_rate < 1.0 {
            warn!("sample_rate must be greater than 1.0");
            return Err(anyhow::anyhow!("sample_rate must be greater than 1.0"));
        }
        if sample_count > sample_rate as usize {
            error!("sample_count must be less than or equal to sample_rate");
            return Err(anyhow!("sample_count must be less than or equal to sample_rate"));
        }
        if sample_array_default.is_some() {
            if sample_array_default.as_ref().unwrap().len() != sample_count {
                error!("sample_array_default length must be equal to sample_count");
                return Err(anyhow!("sample_array_default length must be equal to sample_count"));
            }
        }
        Ok(Self {
            amplitude,
            frequency,
            sample_rate,
            sample_count,
            phase_offset,
            sample_array_default,
        })
    }
}

struct Dac {
    config: Arc<Mutex<DacConfig>>,
    command_rx: Receiver<DacCommand>,
    data_tx: tokio::sync::mpsc::Sender<Vec<f32>>,
    running: Arc<Mutex<bool>>,
}

#[derive(Debug, Clone)]
pub enum DacCommand {
    SetAmplitude(f32),
    SetFrequency(f32),
    SetPhaseOffset(f32),
    SetDefaultSampleArray(Vec<f32>),
    SetSampleRate(f32),
    SetSampleCount(usize),
    Start,
    Stop,
}

impl Dac {
    fn new(config: Arc<Mutex<DacConfig>>, command_rx: Receiver<DacCommand>, data_tx: tokio::sync::mpsc::Sender<Vec<f32>>) -> Self {
        Self { 
            config, 
            command_rx, 
            data_tx,
            running: Arc::new(Mutex::new(false)),
        }
    }
    async fn run(&mut self) {
        let mut sample_rate = self.config.lock().await.sample_rate;
        let interval_duration = Duration::from_millis(1); // This should be adaptive
        let start_time = Instant::now() + interval_duration;
        let mut sample_timer = interval_at(start_time, interval_duration);
        
        loop {
            tokio::select! {
                // Handle commands
                Some(command) = self.command_rx.recv() => {
                    match command {
                        DacCommand::SetAmplitude(amplitude) => {
                            self.config.lock().await.amplitude = amplitude;
                            info!("DAC amplitude set to: {}", amplitude);
                        },
                        DacCommand::SetFrequency(frequency) => {
                            self.config.lock().await.frequency = frequency;
                            info!("DAC frequency set to: {} Hz", frequency);
                        },
                        DacCommand::SetPhaseOffset(phase_offset) => {
                            self.config.lock().await.phase_offset = phase_offset;
                            info!("DAC phase offset set to: {} radians", phase_offset);
                        },
                        DacCommand::SetDefaultSampleArray(samples) => {
                            self.config.lock().await.sample_array_default = Some(samples);
                            info!("DAC default sample array updated");
                        },
                        DacCommand::SetSampleRate(new_sample_rate) => {
                            self.config.lock().await.sample_rate = new_sample_rate;
                            sample_rate = new_sample_rate;
                            // Recalculate timer interval for new sample rate with smooth transition
                            let new_interval = Duration::from_micros(100_000_000 / sample_rate as u64);
                            let next_tick_time = Instant::now() + new_interval;
                            sample_timer = interval_at(next_tick_time, new_interval);
                            info!("DAC sample rate updated to: {}, timer interval: {} μs", 
                                  sample_rate, new_interval.as_micros());
                        },
                        DacCommand::SetSampleCount(new_sample_count) => {
                            self.config.lock().await.sample_count = new_sample_count;
                            info!("DAC sample count updated to: {}", new_sample_count);
                        },
                        DacCommand::Start => {
                            *self.running.lock().await = true;
                            info!("DAC started");
                        },
                        DacCommand::Stop => {
                            *self.running.lock().await = false;
                            info!("DAC stopped");
                        },
                    }
                },
                
                // Generate and transmit samples when running, TODO: this is holy grail for performance, faster the sample rate control channel needs to be bigger and faster
                _ = sample_timer.tick() => {
                    if *self.running.lock().await {
                        let config = self.config.lock().await;
                        let samples = self.generate_samples(&config).await;
                        
                        // Send samples to data channel
                        if let Err(e) = self.data_tx.send(samples).await {
                            error!("Failed to send DAC samples: {}", e);
                        }
                    }
                }
            }
        }
    }
    
    async fn generate_samples(&self, config: &DacConfig) -> Vec<f32> {
        let mut samples = Vec::with_capacity(config.sample_count);
        let sample_period = 1.0 / config.sample_rate;
        
        // Use custom sample array if available, otherwise generate sine wave
        if let Some(ref custom_samples) = config.sample_array_default {
            for i in 0..config.sample_count {
                let t = i as f32 * sample_period;
                let phase = 2.0 * std::f32::consts::PI * config.frequency * t + config.phase_offset;
                let amplitude_index = (i as f32 * custom_samples.len() as f32 / config.sample_count as f32) as usize;
                let minimum_amplitude = custom_samples[amplitude_index].min(config.amplitude);
                let sample = minimum_amplitude * phase.sin();
                samples.push(sample);
            }
        } else {
            // Generate sine wave samples
            for i in 0..config.sample_count {
                let t = i as f32 * sample_period;
                let phase = 2.0 * std::f32::consts::PI * config.frequency * t + config.phase_offset;
                let sample = config.amplitude * phase.sin();
                samples.push(sample);
            }
        }
        return samples;
    }
}
pub async fn dac_interface(mut control_rx: Receiver<ControlCommand>, dac_data_tx: BroadcastSender<Vec<f32>>, runtime: Arc<Runtime>) -> Result<(), anyhow::Error> {
    let dac_config = match DacConfig::new(3.3, 1000.0, 1_000_000.0, 1000, 0.0, None) {
        Ok(config) => config,
        Err(e) => {
            error!("Failed to create DAC config: {:?}", e);
            return Err(anyhow!("Failed to create DAC config: {:?}", e));
        }
    };
    let (dac_command_tx, dac_command_rx) = tokio::sync::mpsc::channel::<DacCommand>(4); // trampoline task to translate User commands to DAC specific commands
    let (dac_data_tx_local, mut dac_data_rx_local) = tokio::sync::mpsc::channel::<Vec<f32>>(4);
    let mut dac = Dac::new(Arc::new(Mutex::new(dac_config)), dac_command_rx, dac_data_tx_local);
    
    // Spawn DAC task
    let dac_task = runtime.spawn(async move {dac.run().await});
    let control_task = runtime.spawn(async move {
        info!("DAC interface control task started");
        loop{
            tokio::select! {
                Some(command) = control_rx.recv() => {
                    match command {
                        ControlCommand::SetAmplitude(amplitude) => {
                            if let Err(e) = dac_command_tx.send(DacCommand::SetAmplitude(amplitude)).await {
                                error!("Failed to send SetAmplitude command: {:?}", e);
                            }
                            info!("SetAmplitude command sent to DAC");
                        },
                        ControlCommand::SetFrequency(frequency) => {
                            if let Err(e) = dac_command_tx.send(DacCommand::SetFrequency(frequency)).await {
                                error!("Failed to send SetFrequency command: {}", e);
                            }
                            info!("SetFrequency command sent to DAC");
                        },
                        ControlCommand::Start => {
                            if let Err(e) = dac_command_tx.send(DacCommand::Start).await {
                                error!("Failed to send Start command: {}", e);
                            }
                            info!("Start command sent to DAC");
                        },
                        ControlCommand::Stop => {
                            if let Err(e) = dac_command_tx.send(DacCommand::Stop).await {
                                error!("Failed to send Stop command: {}", e);
                            }
                            info!("Stop command sent to DAC");
                        },
                        ControlCommand::SetSamplingRate(sample_rate) => {
                            if let Err(e) = dac_command_tx.send(DacCommand::SetSampleRate(sample_rate)).await {
                                error!("Failed to send SetSampleRate command: {}", e);
                            }
                            info!("SetSampleRate command sent to DAC");
                        },
                        ControlCommand::SetSamplingCount(sample_count) => {
                            if let Err(e) = dac_command_tx.send(DacCommand::SetSampleCount(sample_count)).await {
                                error!("Failed to send SetSamplingCount command: {}", e);
                            }
                            info!("SetSamplingCount command sent to DAC");
                        },
                        _ => {
                            info!("Unhandled control command: {:?}", command);
                        }
                    }
                },
                Some(samples) = dac_data_rx_local.recv() => {
                    if let Err(e) = dac_data_tx.send(samples) {
                        error!("Failed to forward samples to broadcast channel: {:?}", e);
                        break;
                    }
                }
            }
        }
    });
    
    let dac_task_result = dac_task.await;
    let control_task_result = control_task.await;
    info!("DAC task result: {:?}", dac_task_result);
    info!("Control task result: {:?}", control_task_result);
    if dac_task_result.is_err() {
        error!("DAC task failed: {:?}", dac_task_result);
        return Err(anyhow!("DAC task failed: {:?}", dac_task_result));
    }
    if control_task_result.is_err() {
        error!("Control task failed: {:?}", control_task_result);
        return Err(anyhow!("Control task failed: {:?}", control_task_result));
    }
    info!("DAC interface tasks completed");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use tokio::sync::broadcast;
    use tokio::runtime::Runtime;
    use tokio::time::Duration;
    use std::sync::Arc;
    use tracing_subscriber;

    #[test]
    fn test_dac_config_default() {
        let config = DacConfig::default();
        assert_eq!(config.amplitude, 3.3);
        assert_eq!(config.frequency, 1000.0);
        assert_eq!(config.sample_rate, 1_000_000.0);
        assert_eq!(config.sample_count, 1000);
        assert_eq!(config.phase_offset, 0.0);
        assert!(config.sample_array_default.is_none());
    }

    #[test]
    fn test_dac_config_new_valid() {
        let config = DacConfig::new(2.5, 500.0, 1_000_000.0, 2000, 1.57, None);
        assert!(config.is_ok());
        let config = config.unwrap();
        assert_eq!(config.amplitude, 2.5);
        assert_eq!(config.frequency, 500.0);
        assert_eq!(config.sample_rate, 1_000_000.0);
        assert_eq!(config.sample_count, 2000);
        assert_eq!(config.phase_offset, 1.57);
    }

    #[test]
    fn test_dac_config_invalid_sample_count() {
        let config = DacConfig::new(2.5, 500.0, 1_000_000.0, 0, 0.0, None);
        assert!(config.is_err());
    }

    #[test]
    fn test_dac_config_invalid_sample_rate() {
        let config = DacConfig::new(2.5, 500.0, 0.5, 1000, 0.0, None);
        assert!(config.is_err());
    }

    #[test]
    fn test_dac_config_sample_count_exceeds_rate() {
        let config = DacConfig::new(2.5, 500.0, 1000.0, 2000, 0.0, None);
        assert!(config.is_err());
    }

    #[test]
    fn test_dac_config_custom_array_length_mismatch() {
        let custom_array = vec![1.0, 2.0, 3.0]; // Length 3, but sample_count is 1000
        let config = DacConfig::new(2.5, 500.0, 1_000_000.0, 1000, 0.0, Some(custom_array));
        assert!(config.is_err());
    }

    #[test]
    fn test_dac_config_custom_array_valid() {
        let custom_array = vec![1.0; 1000]; // Length matches sample_count
        let config = DacConfig::new(2.5, 500.0, 1_000_000.0, 1000, 0.0, Some(custom_array));
        assert!(config.is_ok());
        assert!(config.unwrap().sample_array_default.is_some());
    }

    #[tokio::test]
    async fn test_dac_command_handling() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
        let config = Arc::new(Mutex::new(DacConfig::default()));
        let (command_tx, command_rx) = mpsc::channel(10);
        let (data_tx, _data_rx) = mpsc::channel(10);
        
        let mut dac = Dac::new(config.clone(), command_rx, data_tx);
        
        // Spawn the DAC run loop in a separate task
        let dac_config = config.clone();
        let dac_running = dac.running.clone();
        let _dac_task = tokio::spawn(async move {
            dac.run().await;
        });
        
        // Test SetAmplitude command
        command_tx.send(DacCommand::SetAmplitude(2.5)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await; // Give time for processing
        assert_eq!(dac_config.lock().await.amplitude, 2.5);
        
        // Test SetFrequency command
        command_tx.send(DacCommand::SetFrequency(1500.0)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(dac_config.lock().await.frequency, 1500.0);
        
        // Test SetPhaseOffset command
        command_tx.send(DacCommand::SetPhaseOffset(3.14)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(dac_config.lock().await.phase_offset, 3.14);
        
        // Test SetSampleRate command
        command_tx.send(DacCommand::SetSampleRate(10_000.0)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(dac_config.lock().await.sample_rate, 10_000.0);
        
        // Test SetSampleCount command
        command_tx.send(DacCommand::SetSampleCount(1000)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(dac_config.lock().await.sample_count, 1000);
        // Test Start/Stop commands
        command_tx.send(DacCommand::Start).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(*dac_running.lock().await);
        
        command_tx.send(DacCommand::Stop).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!*dac_running.lock().await);
    }

    #[tokio::test]
    async fn test_sample_generation_sine_wave() {
        let config = DacConfig {
            amplitude: 1.0,
            frequency: 1000.0,
            sample_rate: 10_000.0, // 10 KSPS
            sample_count: 10,
            phase_offset: 0.0,
            sample_array_default: None,
        };
        
        let dac = Dac::new(
            Arc::new(Mutex::new(config.clone())),
            mpsc::channel(1).1,
            mpsc::channel(1).0,
        );
        
        let samples = dac.generate_samples(&config).await;
        
        assert_eq!(samples.len(), 10);
        
        // Check that samples follow sine wave pattern
        let sample_period = 1.0 / config.sample_rate;
        for (i, &sample) in samples.iter().enumerate() {
            let t = i as f32 * sample_period;
            let expected = config.amplitude * (2.0 * std::f32::consts::PI * config.frequency * t).sin();
            assert!((sample - expected).abs() < 1e-6, "Sample {}: expected {}, got {}", i, expected, sample);
        }
    }

    #[tokio::test]
    async fn test_sample_generation_with_custom_array() {
        let custom_array = vec![0.5, 1.0, 0.5, 0.0, 0.5, 1.0, 0.5, 0.0, 0.5, 1.0];
        let config = DacConfig {
            amplitude: 2.0,
            frequency: 1000.0,
            sample_rate: 10_000.0,
            sample_count: 10,
            phase_offset: 0.0,
            sample_array_default: Some(custom_array.clone()),
        };
        
        let dac = Dac::new(
            Arc::new(Mutex::new(config.clone())),
            mpsc::channel(1).1,
            mpsc::channel(1).0,
        );
        
        let samples = dac.generate_samples(&config).await;
        
        assert_eq!(samples.len(), 10);
        
        // Check that samples use custom amplitude envelope
        let sample_period = 1.0 / config.sample_rate;
        for (i, &sample) in samples.iter().enumerate() {
            let t = i as f32 * sample_period;
            let phase = 2.0 * std::f32::consts::PI * config.frequency * t;
            let amplitude_index = (i as f32 * custom_array.len() as f32 / config.sample_count as f32) as usize;
            let min_amplitude = custom_array[amplitude_index].min(config.amplitude);
            let expected = min_amplitude * phase.sin();
            assert!((sample - expected).abs() < 1e-6, "Sample {}: expected {}, got {}", i, expected, sample);
        }
    }

    #[tokio::test]
    async fn test_sample_generation_with_phase_offset() {
        let config = DacConfig {
            amplitude: 1.0,
            frequency: 1000.0,
            sample_rate: 10_000.0,
            sample_count: 10,
            phase_offset: std::f32::consts::PI / 2.0, // 90 degrees
            sample_array_default: None,
        };
        
        let dac = Dac::new(
            Arc::new(Mutex::new(config.clone())),
            mpsc::channel(1).1,
            mpsc::channel(1).0,
        );
        
        let samples = dac.generate_samples(&config).await;
        
        assert_eq!(samples.len(), 10);
        
        // Check that samples include phase offset
        let sample_period = 1.0 / config.sample_rate;
        for (i, &sample) in samples.iter().enumerate() {
            let t = i as f32 * sample_period;
            let expected = config.amplitude * (2.0 * std::f32::consts::PI * config.frequency * t + config.phase_offset).sin();
            assert!((sample - expected).abs() < 1e-6, "Sample {}: expected {}, got {}", i, expected, sample);
        }
    }

    #[tokio::test]
    async fn test_timer_interval_calculation() {
        let sample_rate = 1_000_000.0; // 1 MSPS
        let interval_duration = Duration::from_nanos(1000000000 / sample_rate as u64);
        
        assert_eq!(interval_duration.as_nanos(), 1000); // Should be 1000 nanoseconds = 1 microsecond
        
        let sample_rate_2 = 500_000.0; // 500 KSPS
        let interval_duration_2 = Duration::from_nanos(1000000000 / sample_rate_2 as u64);
        
        assert_eq!(interval_duration_2.as_nanos(), 2000); // Should be 2000 nanoseconds = 2 microseconds
    }

    #[tokio::test]
    async fn test_dac_interface_integration() {
        // Initialize tracing for the test
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
            
        let runtime = Runtime::new().unwrap();
        let (control_tx, control_rx) = mpsc::channel(10);
        let (dac_data_tx, mut dac_data_rx) = broadcast::channel(10);
        
        // Spawn the DAC interface
        let arc_runtime = Arc::new(runtime);
        let runtime_clone = arc_runtime.clone();
        let runtime_1 = arc_runtime.clone();
        let dac_task = runtime_1.spawn(async move {
            dac_interface(control_rx, dac_data_tx, runtime_clone).await
        });
        
        // Send some control commands
        control_tx.send(ControlCommand::SetAmplitude(2.5)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        control_tx.send(ControlCommand::SetFrequency(1500.0)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        control_tx.send(ControlCommand::SetSamplingRate(1_000_000.0)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        control_tx.send(ControlCommand::Start).await.unwrap();
        
        // Wait a bit for samples to be generated
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        
        let mut sample_count = 0;
        let start_time = std::time::Instant::now();
        let max_duration = Duration::from_millis(500);
        
        loop {
            match dac_data_rx.try_recv() {
                Ok(samples) => {
                    sample_count += samples.len();
                },
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    if start_time.elapsed() > max_duration {
                        info!("Timeout reached, received {} total samples", sample_count);
                        break;
                    }
                    tokio::task::yield_now().await; // Minimal overhead
                },
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                    error!("Broadcast receiver lagged by {} messages", n);
                },
                Err(e) => {
                    error!("Broadcast error: {:?}", e);
                    break;
                }
            }
        }
        
        // Should have received some samples
        
        // Stop the DAC
        control_tx.send(ControlCommand::Stop).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        info!("Stopped DAC");

        assert!(sample_count > 0, "Expected to receive samples, but got {}", sample_count);

        // Clean up
        drop(control_tx);
        tokio::time::sleep(Duration::from_millis(10)).await;
        dac_task.abort();
    }

    #[tokio::test]
    async fn test_dac_running_state() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();

        let config = Arc::new(Mutex::new(DacConfig::default()));
        let (command_tx, command_rx) = mpsc::channel(10);
        let (data_tx, _data_rx) = mpsc::channel(10);
        
        let mut dac = Dac::new(config, command_rx, data_tx);
        let dac_running = dac.running.clone();
        
        // Initially not running
        assert!(!*dac_running.lock().await);

        let dac_task = tokio::spawn(async move {
            dac.run().await;
        });
        // Start DAC
        info!("Starting DAC");
        command_tx.send(DacCommand::Start).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(*dac_running.lock().await);

        // Stop DAC
        command_tx.send(DacCommand::Stop).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!*dac_running.lock().await);
        info!("Stopped DAC");

        drop(command_tx);
        tokio::time::sleep(Duration::from_millis(10)).await;
        dac_task.abort();
    }

    #[test]
    fn test_dac_command_variants() {
        // Test that all command variants can be created
        let _cmd1 = DacCommand::SetAmplitude(1.0);
        let _cmd2 = DacCommand::SetFrequency(1000.0);
        let _cmd3 = DacCommand::SetPhaseOffset(0.5);
        let _cmd4 = DacCommand::SetDefaultSampleArray(vec![1.0, 2.0, 3.0]);
        let _cmd5 = DacCommand::SetSampleRate(1_000_000.0);
        let _cmd6 = DacCommand::SetSampleCount(1000);
        let _cmd7 = DacCommand::Start;
        let _cmd8 = DacCommand::Stop;
    }
}