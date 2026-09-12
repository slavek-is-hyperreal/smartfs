//! Pure mathematical functions for centroid updates, k-means clustering (k=2),
//! and variance combination (Chan et al.).

use crate::types::{CentroidMemberWithVector, ConceptCentroid};
use smartfs_schema::error::{Result, SmartFsError};
use uuid::Uuid;

/// @id: 4e819b22-834c-47ea-a2f0-15949d211a7c
/// Calculates the cosine distance between two float vectors: `1.0 - (dot / (norm_a * norm_b))`.
///
/// Returns 1.0 if either vector has a zero norm. Result is clamped to `[0.0, 2.0]`.
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || a.len() != b.len() {
        return 1.0;
    }
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;

    for (&x, &y) in a.iter().zip(b.iter()) {
        let x64 = x as f64;
        let y64 = y as f64;
        dot += x64 * y64;
        norm_a += x64 * x64;
        norm_b += y64 * y64;
    }

    if norm_a <= 0.0 || norm_b <= 0.0 {
        return 1.0;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom <= 0.0 {
        return 1.0;
    }

    let sim = dot / denom;
    (1.0 - sim).clamp(0.0, 2.0)
}

/// @id: f0b7d2e4-8a3c-4f19-9d5e-6c1a0b7f3d88
/// Incremental mean and M2 (sum of squared deviations) calculation using Welford's algorithm.
#[allow(clippy::ptr_arg)]
pub fn welford_update(mean: &mut Vec<f32>, m2: &mut f64, count: &mut i64, new_point: &[f32]) {
    *count += 1;
    let n = *count as f32;
    let mut sq_delta_sum = 0.0f64;
    for (m, x) in mean.iter_mut().zip(new_point) {
        let delta = x - *m;
        *m += delta / n;
        let delta2 = x - *m;
        sq_delta_sum += (delta * delta2) as f64;
    }
    *m2 += sq_delta_sum;
}

/// @id: 2e9c5a17-4d80-4b3e-8f21-7a6d0c9b5e33
/// Computes sample variance from M2 and count. Returns 0.0 if count < 2.
pub fn variance_from_m2(m2: f64, count: i64) -> f64 {
    if count < 2 {
        0.0
    } else {
        m2 / (count as f64 - 1.0)
    }
}

/// @id: d1f6a3c8-7e02-4b95-a3d1-6f8c2e0b9a55
/// Result of local k-means (k=2) or parallel variance centroid combination.
/// Contains all parameters needed to persist or create new centroids without full recalculation.
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    pub mean: Vec<f32>,
    pub m2: f64,
    pub count: i64,
    pub member_ids: Vec<Uuid>,
    pub member_distances: Vec<f64>,
}

/// @id: 5c8e2a94-1f6d-4b37-a8c0-3e7f9d2b6a15
/// Lloyd's algorithm for k=2.
///
/// Invariant:
/// - Initialized with the two most distant points (2-approximation).
/// - If all points are identical (distance == 0.0), partitions points evenly and returns identical means without error.
/// - Returns `(Cluster, Cluster)`.
pub fn kmeans2(members: &[CentroidMemberWithVector]) -> Result<(Cluster, Cluster)> {
    if members.len() < 2 {
        return Err(SmartFsError::Other(
            "kmeans2 requires at least 2 members to perform a split".to_string(),
        ));
    }

    let dim = members[0].vector.len();
    for m in members {
        if m.vector.len() != dim {
            return Err(SmartFsError::Other(
                "Inconsistent vector dimensions in kmeans2 members".to_string(),
            ));
        }
    }

    // Find the pair with maximum distance for Lloyd k=2 initialization
    let mut max_dist = -1.0f64;
    let mut init_i = 0;
    let mut init_j = 1;

    for i in 0..members.len() {
        for j in (i + 1)..members.len() {
            let d = cosine_distance(&members[i].vector, &members[j].vector);
            if d > max_dist {
                max_dist = d;
                init_i = i;
                init_j = j;
            }
        }
    }

    // Degenerate case: all points have identical coordinates
    if max_dist <= 1e-9 {
        let mid = members.len() / 2;
        let (first_half, second_half) = members.split_at(mid);

        let mean = members[0].vector.clone();
        let cluster_a = Cluster {
            mean: mean.clone(),
            m2: 0.0,
            count: first_half.len() as i64,
            member_ids: first_half.iter().map(|m| m.id).collect(),
            member_distances: vec![0.0; first_half.len()],
        };
        let cluster_b = Cluster {
            mean,
            m2: 0.0,
            count: second_half.len() as i64,
            member_ids: second_half.iter().map(|m| m.id).collect(),
            member_distances: vec![0.0; second_half.len()],
        };
        return Ok((cluster_a, cluster_b));
    }

    // Initialize means
    let mut mean0 = members[init_i].vector.clone();
    let mut mean1 = members[init_j].vector.clone();

    let mut assignments = vec![0usize; members.len()];
    let max_iter = 50;

    for _ in 0..max_iter {
        let mut changed = false;
        let mut count0 = 0usize;
        let mut count1 = 0usize;

        for (k, m) in members.iter().enumerate() {
            let d0 = cosine_distance(&m.vector, &mean0);
            let d1 = cosine_distance(&m.vector, &mean1);
            let new_assign = if d0 <= d1 { 0 } else { 1 };
            if new_assign != assignments[k] {
                assignments[k] = new_assign;
                changed = true;
            }
            if new_assign == 0 {
                count0 += 1;
            } else {
                count1 += 1;
            }
        }

        // Handle empty cluster edge cases
        if count0 == 0 {
            // Find point in cluster 1 farthest from mean1 and move to cluster 0
            let mut farthest_idx = 0;
            let mut farthest_dist = -1.0;
            for (k, m) in members.iter().enumerate() {
                let d = cosine_distance(&m.vector, &mean1);
                if d > farthest_dist {
                    farthest_dist = d;
                    farthest_idx = k;
                }
            }
            assignments[farthest_idx] = 0;
            changed = true;
        } else if count1 == 0 {
            // Find point in cluster 0 farthest from mean0 and move to cluster 1
            let mut farthest_idx = 0;
            let mut farthest_dist = -1.0;
            for (k, m) in members.iter().enumerate() {
                let d = cosine_distance(&m.vector, &mean0);
                if d > farthest_dist {
                    farthest_dist = d;
                    farthest_idx = k;
                }
            }
            assignments[farthest_idx] = 1;
            changed = true;
        }

        if !changed {
            break;
        }

        // Recompute means
        let mut sum0 = vec![0.0f64; dim];
        let mut sum1 = vec![0.0f64; dim];
        let mut n0 = 0usize;
        let mut n1 = 0usize;

        for (k, m) in members.iter().enumerate() {
            if assignments[k] == 0 {
                n0 += 1;
                for (s, &v) in sum0.iter_mut().zip(&m.vector) {
                    *s += v as f64;
                }
            } else {
                n1 += 1;
                for (s, &v) in sum1.iter_mut().zip(&m.vector) {
                    *s += v as f64;
                }
            }
        }

        if n0 > 0 {
            for (m, &s) in mean0.iter_mut().zip(&sum0) {
                *m = (s / n0 as f64) as f32;
            }
        }
        if n1 > 0 {
            for (m, &s) in mean1.iter_mut().zip(&sum1) {
                *m = (s / n1 as f64) as f32;
            }
        }
    }

    // Build clusters with final metrics
    let build_cluster = |assign_val: usize, mean: Vec<f32>| -> Cluster {
        let mut member_ids = Vec::new();
        let mut member_distances = Vec::new();
        let mut c_mean = vec![0.0f32; dim];
        let mut c_m2 = 0.0f64;
        let mut c_count = 0i64;

        for (k, m) in members.iter().enumerate() {
            if assignments[k] == assign_val {
                member_ids.push(m.id);
                let dist = cosine_distance(&m.vector, &mean);
                member_distances.push(dist);

                if c_count == 0 {
                    c_mean = m.vector.clone();
                    c_count = 1;
                } else {
                    welford_update(&mut c_mean, &mut c_m2, &mut c_count, &m.vector);
                }
            }
        }

        Cluster {
            mean: if c_count > 0 { c_mean } else { mean },
            m2: c_m2,
            count: c_count,
            member_ids,
            member_distances,
        }
    };

    let cluster_a = build_cluster(0, mean0);
    let cluster_b = build_cluster(1, mean1);

    Ok((cluster_a, cluster_b))
}

/// @id: 8d7c4b12-9e23-4f51-b0a7-3e8a5c1d6b99
/// Combines two centroids into a single Cluster using Chan et al.'s parallel variance algorithm.
///
/// Formula:
/// - `mean_AB = (n_A * mean_A + n_B * mean_B) / (n_A + n_B)`
/// - `M2_AB = M2_A + M2_B + (n_A * n_B / (n_A + n_B)) * delta^2`
pub fn combine_centroids(a: &ConceptCentroid, b: &ConceptCentroid) -> Cluster {
    let n_a = a.member_count;
    let n_b = b.member_count;
    let total_count = n_a + n_b;

    if total_count == 0 {
        return Cluster {
            mean: a.centroid.clone(),
            m2: 0.0,
            count: 0,
            member_ids: Vec::new(),
            member_distances: Vec::new(),
        };
    }

    if n_a == 0 {
        return Cluster {
            mean: b.centroid.clone(),
            m2: b.m2,
            count: n_b,
            member_ids: Vec::new(),
            member_distances: Vec::new(),
        };
    }

    if n_b == 0 {
        return Cluster {
            mean: a.centroid.clone(),
            m2: a.m2,
            count: n_a,
            member_ids: Vec::new(),
            member_distances: Vec::new(),
        };
    }

    let dim = a.centroid.len();
    let n_a_f = n_a as f64;
    let n_b_f = n_b as f64;
    let total_f = total_count as f64;

    let mut combined_mean = Vec::with_capacity(dim);
    let mut delta_sq = 0.0f64;

    for i in 0..dim {
        let val_a = a.centroid[i] as f64;
        let val_b = b.centroid[i] as f64;
        let mean_val = (n_a_f * val_a + n_b_f * val_b) / total_f;
        combined_mean.push(mean_val as f32);

        let delta = val_a - val_b;
        delta_sq += delta * delta;
    }

    let parallel_term = (n_a_f * n_b_f / total_f) * delta_sq;
    let combined_m2 = a.m2 + b.m2 + parallel_term;

    Cluster {
        mean: combined_mean,
        m2: combined_m2,
        count: total_count,
        member_ids: Vec::new(),
        member_distances: Vec::new(),
    }
}
