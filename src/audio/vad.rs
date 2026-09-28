use std::borrow::Cow;

use anyhow::Context;
use ndarray::{Array1, Array3};
use ort::ep::cpu::CPU as CPUExecutionProvider;
use ort::session::builder::SessionBuilder;
use ort::session::{SessionInputValue, SessionInputs};
use ort::value::{Tensor, Value};

pub struct SileroVad {
    session: ort::session::Session,
    state: Array3<f32>,  // [2, 1, 128] — combined LSTM state (silero v5)
    #[allow(dead_code)]
    sr: Array1<i64>,     // [1]
}

// ort::Session contains a raw pointer internally; we only ever access it from
// the single LocalSet thread, so this is safe.
unsafe impl Send for SileroVad {}

impl SileroVad {
    pub fn new(model_path: &std::path::Path) -> anyhow::Result<Self> {
        let providers = [CPUExecutionProvider::default().build()];
        let session = SessionBuilder::new()
            .map_err(|e| anyhow::anyhow!("Failed to create ORT session builder: {e}"))?
            .with_execution_providers(providers)
            .map_err(|e| anyhow::anyhow!("Failed to set execution providers: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| anyhow::anyhow!("Failed to load Silero VAD model: {e}"))?;

        Ok(Self {
            session,
            state: Array3::zeros([2, 1, 128]),
            sr: Array1::from_vec(vec![16000i64]),
        })
    }

    /// Process one 512-sample chunk (32 ms at 16 kHz).
    /// Returns speech probability in the range 0.0–1.0.
    pub fn process(&mut self, samples: &[f32; 512]) -> anyhow::Result<f32> {
        // ---- input [1, 512] f32 ----
        let input_data: Vec<f32> = samples.to_vec();
        let input_tensor =
            Tensor::from_array(([1usize, 512], input_data)).context("Failed to create input tensor")?;

        // ---- state [2, 1, 128] f32 ----
        let state_data: Vec<f32> = self.state.iter().cloned().collect();
        let state_tensor =
            Tensor::from_array(([2usize, 1, 128], state_data)).context("Failed to create state tensor")?;

        // ---- sr [1] i64 ----
        let sr_tensor =
            Tensor::from_array(([1usize], vec![16000i64])).context("Failed to create sr tensor")?;

        let inputs: Vec<(Cow<str>, SessionInputValue)> = vec![
            (
                Cow::Borrowed("input"),
                SessionInputValue::Owned(Value::from(input_tensor)),
            ),
            (
                Cow::Borrowed("state"),
                SessionInputValue::Owned(Value::from(state_tensor)),
            ),
            (
                Cow::Borrowed("sr"),
                SessionInputValue::Owned(Value::from(sr_tensor)),
            ),
        ];

        let outputs = self
            .session
            .run(SessionInputs::from(inputs))
            .map_err(|e| anyhow::anyhow!("Failed to run Silero VAD session: {e}"))?;

        // output[0] → speech probability [1, 1] f32
        let (_, output_data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("Failed to extract output tensor: {e}"))?;
        let prob = output_data[0];

        // output[1] → stateN [2, 1, 128] f32 — updated LSTM state
        let (sn_shape, sn_data) = outputs[1]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("Failed to extract stateN tensor: {e}"))?;
        let sn_dims: Vec<usize> = sn_shape.iter().map(|&d| d as usize).collect();
        self.state =
            Array3::from_shape_vec([sn_dims[0], sn_dims[1], sn_dims[2]], sn_data.to_vec())
                .context("Failed to reshape stateN")?;

        Ok(prob)
    }

    /// Reset the LSTM hidden state (call at the start of a new session).
    pub fn reset(&mut self) {
        self.state = Array3::zeros([2, 1, 128]);
    }
}
