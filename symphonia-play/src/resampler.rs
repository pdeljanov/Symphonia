use rubato::FftFixedIn;
use symphonia::core::audio::SignalSpec;

pub struct Resampler
{
    resampler:FftFixedIn<f32>,
    output_buffer:Vec<Vec<f32>>,
    num_frames:usize,
    num_channels:usize
}

impl Resampler
{
    pub fn new(spec:SignalSpec, to_sample_rate:usize, num_frames:usize) -> Self
    {
        let num_channels = spec.channels.count();

        let resampler = FftFixedIn::<f32>::new(
            spec.rate as usize,
            to_sample_rate,
            num_frames,
            2,
            num_channels
        ).unwrap();

        let output_buffer = rubato::Resampler::output_buffer_allocate(&resampler);

        Self
        {
            resampler,
            output_buffer,
            num_frames,
            num_channels
        }
    }

    /// Resamples a planar/non-interleaved input.
    /// 
    /// Returns the resampled samples in an interleaved format.
    pub fn resample(&mut self, input:&[f32]) -> Vec<f32>
    {
        // The `input` is represented like so: LLLLLLRRRRRR
        // To resample this input, we split the channels (L, R) into 2 vectors.
        // The input now becomes [[LLLLLL], [RRRRRR]].
        // This is what `rubato` needs.
        let mut planar:Vec<Vec<f32>> = vec![Vec::new(); self.num_channels];

        let mut offset = 0;
        for channel in 0..self.num_channels
        {
            planar[channel] = input[offset..offset + self.num_frames].to_vec();
            offset += self.num_frames;
        }

        rubato::Resampler::process_into_buffer(
            &mut self.resampler,
            &planar,
            &mut self.output_buffer,
            None
        ).unwrap();

        // The `interleaved` samples are represented like so: LRLRLRLRLRLR
        let mut interleaved:Vec<f32> = vec![0.0; self.output_buffer[0].len() * self.num_channels];

        // Interleave all the samples of each channel.
        let mut current_frame = 0;
        for frame in interleaved.chunks_exact_mut(self.num_channels)
        {
            for channel in 0..self.num_channels
            {
                frame[channel] = self.output_buffer[channel][current_frame];
            }

            current_frame += 1;
        }

        interleaved
    }
}