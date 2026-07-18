pub fn mean_pool(
    hidden_states: &[f32],
    attention_mask: &[i32],
    seq_len: usize,
    dim: usize,
) -> Vec<f32> {
    let mut sum = vec![0.0f32; dim];
    let mut count = 0.0f32;

    for pos in 0..seq_len {
        if attention_mask.get(pos).copied().unwrap_or(0) > 0 {
            let offset = pos * dim;
            for (d, s) in sum.iter_mut().enumerate() {
                if let Some(&val) = hidden_states.get(offset + d) {
                    *s += val;
                }
            }
            count += 1.0;
        }
    }

    if count > 0.0 {
        for val in &mut sum {
            *val /= count;
        }
    }
    sum
}
