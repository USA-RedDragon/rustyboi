pub trait AudioOutput {
    fn start(&mut self) -> Result<(), Box<dyn std::error::Error>>;
    fn add_samples(&mut self, samples: &[(f32, f32)]);
}