use crate::ControlCommand;
use tokio::sync::mpsc::{Sender, Receiver};


pub async fn adc_interface(_control_rx: Receiver<ControlCommand>, _dac_data_rx: Receiver<Vec<f32>>, _adc_data_tx: Sender<Vec<f32>>) {
    let _adc_data = Vec::<f32>::new();
    let _adc_data_buffer = Vec::<f32>::new();
    let _adc_data_buffer_index = 0;
    let _adc_data_buffer_size = 1024;
    let _adc_data_buffer_index = 0;
}