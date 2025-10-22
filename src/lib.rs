mod dac_interface;
mod adc_interface;

// Re-export the main interface functions
pub use dac_interface::dac_interface;

#[derive(Debug, Clone)]
pub enum ControlCommand {
    Start,
    Stop,
    SetAmplitude(f32),
    SetAmplitudeRange(f32, f32),
    SetFrequency(f32),
    SetFrequencyRange(f32, f32),
    SetDeviationTolerance(f32),
    SetDeviationToleranceRange(f32, f32),
    SetSamplingRate(f32),
    SetSamplingCount(usize),
}
