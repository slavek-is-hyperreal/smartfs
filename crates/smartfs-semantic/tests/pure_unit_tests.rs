use smartfs_semantic::*;
use uuid::Uuid;

#[test]
fn test_welford_single_point() {
    let point = vec![1.0f32, 2.0, 3.0];
    let mean = point.clone();
    let m2 = 0.0f64;
    let count = 1i64;

    assert_eq!(variance_from_m2(m2, count), 0.0);
    assert_eq!(count, 1);
    assert_eq!(mean, point);
}

#[test]
fn test_welford_multiple_points() {
    let p1 = vec![1.0f32, 2.0];
    let p2 = vec![3.0f32, 4.0];
    let p3 = vec![5.0f32, 6.0];

    let mut mean = p1.clone();
    let mut m2 = 0.0f64;
    let mut count = 1i64;

    welford_update(&mut mean, &mut m2, &mut count, &p2);
    welford_update(&mut mean, &mut m2, &mut count, &p3);

    assert_eq!(count, 3);
    assert_eq!(mean, vec![3.0, 4.0]);

    // Deviations from mean [3, 4]:
    // p1: (-2)^2 + (-2)^2 = 8
    // p2: 0^2 + 0^2 = 0
    // p3: 2^2 + 2^2 = 8
    // Total sum of squares = 16.0
    assert!((m2 - 16.0).abs() < 1e-5);

    // Sample variance: 16.0 / (3 - 1) = 8.0
    let var = variance_from_m2(m2, count);
    assert!((var - 8.0).abs() < 1e-5);
}

#[test]
fn test_kmeans2_normal() {
    let p1 = CentroidMemberWithVector {
        id: Uuid::new_v4(),
        vector: vec![1.0, 0.0, 0.0],
    };
    let p2 = CentroidMemberWithVector {
        id: Uuid::new_v4(),
        vector: vec![0.9, 0.1, 0.0],
    };
    let p3 = CentroidMemberWithVector {
        id: Uuid::new_v4(),
        vector: vec![0.0, 1.0, 0.0],
    };
    let p4 = CentroidMemberWithVector {
        id: Uuid::new_v4(),
        vector: vec![0.0, 0.9, 0.1],
    };

    let members = vec![p1, p2, p3, p4];
    let (c1, c2) = kmeans2(&members).expect("kmeans2 should succeed");

    assert_eq!(c1.count + c2.count, 4);
    assert_eq!(c1.member_ids.len() as i64, c1.count);
    assert_eq!(c2.member_ids.len() as i64, c2.count);
    assert!(c1.count >= 1);
    assert!(c2.count >= 1);
}

#[test]
fn test_kmeans2_identical_points_degenerate() {
    let p1 = CentroidMemberWithVector {
        id: Uuid::new_v4(),
        vector: vec![0.5, 0.5, 0.5],
    };
    let p2 = CentroidMemberWithVector {
        id: Uuid::new_v4(),
        vector: vec![0.5, 0.5, 0.5],
    };

    let members = vec![p1, p2];
    let (c1, c2) = kmeans2(&members).expect("kmeans2 on identical points should succeed without crash");

    assert_eq!(c1.count, 1);
    assert_eq!(c2.count, 1);
    assert_eq!(c1.mean, c2.mean);
    assert_eq!(c1.m2, 0.0);
    assert_eq!(c2.m2, 0.0);
}

#[test]
fn test_kmeans2_error_on_too_few() {
    let empty: Vec<CentroidMemberWithVector> = Vec::new();
    assert!(kmeans2(&empty).is_err());

    let single = vec![CentroidMemberWithVector {
        id: Uuid::new_v4(),
        vector: vec![1.0, 0.0],
    }];
    assert!(kmeans2(&single).is_err());
}

#[test]
fn test_combine_centroids_chan() {
    let c1 = ConceptCentroid {
        id: Uuid::new_v4(),
        plugin_type: "rust".to_string(),
        model_id: Uuid::new_v4(),
        centroid: vec![1.0, 2.0],
        m2: 0.0,
        member_count: 1,
        label: None,
        is_active: true,
        merged_into: None,
        created_at: None,
        last_consolidated_at: None,
    };

    let c2 = ConceptCentroid {
        id: Uuid::new_v4(),
        plugin_type: "rust".to_string(),
        model_id: c1.model_id,
        centroid: vec![3.0, 4.0],
        m2: 0.0,
        member_count: 1,
        label: None,
        is_active: true,
        merged_into: None,
        created_at: None,
        last_consolidated_at: None,
    };

    let merged = combine_centroids(&c1, &c2);
    assert_eq!(merged.count, 2);
    assert_eq!(merged.mean, vec![2.0, 3.0]);

    // Chan formula: M2_AB = M2_A + M2_B + (1*1/2) * delta_sq
    // delta = [-2, -2] -> delta_sq = 4 + 4 = 8
    // M2_AB = 0 + 0 + 0.5 * 8 = 4.0
    assert!((merged.m2 - 4.0).abs() < 1e-5);
}

#[test]
fn test_advisory_lock_key_postgres_vectors() {
    assert_eq!(pg_hash_bytes(b""), -1477818771);
    assert_eq!(pg_hash_bytes(b"a"), 1075015857);
    assert_eq!(pg_hash_bytes(b"ab"), 1718550461);
    assert_eq!(pg_hash_bytes(b"abc"), -785388649);
    assert_eq!(pg_hash_bytes(b"abcd"), -393934804);
    assert_eq!(pg_hash_bytes(b"abcde"), -445659580);

    let model_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    assert_eq!(advisory_lock_key("rust", model_id), -648330360);
}

#[test]
fn test_tokenize_identifier_cases() {
    assert_eq!(
        tokenize_identifier("parseConfigFile"),
        vec!["parse", "config", "file"]
    );
    assert_eq!(
        tokenize_identifier("read_uncompressed_blob"),
        vec!["read", "uncompressed", "blob"]
    );
    assert_eq!(
        tokenize_identifier("HTMLParser"),
        vec!["html", "parser"]
    );
    assert_eq!(
        tokenize_identifier("build_AST_node"),
        vec!["build", "ast", "node"]
    );
}

#[test]
fn test_merge_by_similarity_dedup_and_sort() {
    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    let id3 = Uuid::new_v4();

    let crystallized = vec![
        ConceptSearchHit {
            id: id1,
            distance: 0.25,
            source: HitSource::Crystallized,
        },
        ConceptSearchHit {
            id: id2,
            distance: 0.50,
            source: HitSource::Crystallized,
        },
    ];

    let buffered = vec![
        // Duplicate of id2 with smaller distance
        ConceptSearchHit {
            id: id2,
            distance: 0.10,
            source: HitSource::Buffered,
        },
        ConceptSearchHit {
            id: id3,
            distance: 0.80,
            source: HitSource::Buffered,
        },
    ];

    let merged = merge_by_similarity(crystallized, buffered, 2);
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].id, id2);
    assert_eq!(merged[0].distance, 0.10);
    assert_eq!(merged[0].source, HitSource::Buffered);
    assert_eq!(merged[1].id, id1);
    assert_eq!(merged[1].distance, 0.25);
}
