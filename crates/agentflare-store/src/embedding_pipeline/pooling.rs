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
            for d in 0..dim {
                if let Some(&val) = hidden_states.get(offset + d) {
                    sum[d] += val;
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

pub fn normalize_l2(vec: &mut [f32]) {
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in vec.iter_mut() {
            *x /= norm;
        }
    }
}

pub fn l2_norm(vec: &[f32]) -> f32 {
    vec.iter().map(|x| x * x).sum::<f32>().sqrt()
}
