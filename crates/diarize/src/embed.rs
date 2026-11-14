//! Turning a window of audio into a vector that describes the voice in it.

use std::path::{Path, PathBuf};

use ort::environment::Environment;
use ort::session::Session;
use ort::value::Tensor;

/// The model file this expects, inside the models directory.
pub const MODEL_FILE: &str = "wespeaker_en_voxceleb_resnet34_LM.onnx";

/// Where the model lives.
#[must_use]
pub fn model_path() -> PathBuf {
    let mut p = dirs_support_dir();
    p.push("models");
    p.push(MODEL_FILE);
    p
}

fn dirs_support_dir() -> PathBuf {
    let mut p = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    p.push("Library/Application Support/readsubtitles");
    p
}

/// A loaded speaker-embedding model.
pub struct Embedder {
    /// Held for the session's lifetime: dropping the environment while a
    /// session is live tears down the runtime underneath it.
    _env: Environment,
    session: Session,
}

impl Embedder {
    /// Load the model from `path`.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            return Err(format!("speaker model not found at {}", path.display()).into());
        }
        let env = ort::init().build()?;
        let session = Session::builder(&env)?.commit_from_file(path)?;
        Ok(Self { _env: env, session })
    }

    /// Names and shapes, for reporting what the model actually wants.
    #[must_use]
    pub fn describe(&self) -> String {
        let ins: Vec<String> = self
            .session
            .inputs()
            .iter()
            .map(|i| format!("{} {:?}", i.name(), i.dtype()))
            .collect();
        let outs: Vec<String> = self
            .session
            .outputs()
            .iter()
            .map(|o| format!("{} {:?}", o.name(), o.dtype()))
            .collect();
        format!("inputs: {}\noutputs: {}", ins.join(", "), outs.join(", "))
    }

    /// Embed a [frames x 80] block of filterbank features.
    pub fn embed(
        &mut self,
        feats: &[f32],
        frames: usize,
        bins: usize,
    ) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        if frames == 0 || feats.len() != frames * bins {
            return Err("feature block is not frames x bins".into());
        }
        let tensor = Tensor::from_array(([1usize, frames, bins], feats.to_vec()))?;
        let outputs = self.session.run(ort::inputs!["feats" => tensor])?;
        let (_, data) = outputs["embs"].try_extract_tensor::<f32>()?;
        Ok(data.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loading and running the real model, when it is present.
    ///
    /// Skips rather than fails when the model has not been downloaded, so the
    /// suite still passes on a machine that has not fetched it.
    #[test]
    fn the_model_loads_and_produces_an_embedding() {
        let path = model_path();
        if !path.exists() {
            eprintln!("skipping: no model at {}", path.display());
            return;
        }
        let mut e = Embedder::load(&path).expect("model should load");
        println!("{}", e.describe());

        // 200 frames is two seconds at the standard 10 ms hop.
        let feats = vec![0.0f32; 200 * 80];
        let embedding = e.embed(&feats, 200, 80).expect("forward pass should run");
        println!("embedding: {} dimensions", embedding.len());
        assert!(!embedding.is_empty(), "model returned no embedding");
        assert!(
            embedding.iter().all(|x| x.is_finite()),
            "embedding contains non-finite values"
        );
    }
}
