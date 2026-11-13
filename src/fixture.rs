//! A deterministic subset of the mock dashboard's data, used for the `demo` scene so headless
//! snapshots and tests don't depend on a real (slow) scan of P:\Projects.

//! A deterministic subset of the mock dashboard's data, used for the `demo` scene so headless
//! snapshots and tests don't depend on a real (slow) scan of P:\Projects.

use crate::model::{Activity, Project};

#[allow(clippy::too_many_arguments)]
fn p(
    name: &str,
    cat: &'static str,
    stack: &str,
    bytes: u64,
    files: u64,
    loc: u64,
    src: u64,
    nm: u64,
    target: u64,
    other: u64,
    commits: u64,
    days: Option<u64>,
    dirty: u64,
    langs: &[(&'static str, u64)],
    act_seed: u64,
) -> Project {
    Project {
        name: name.to_string(),
        cat,
        stack: stack.to_string(),
        bytes,
        files,
        loc,
        src,
        nm,
        target,
        other,
        langs: langs.iter().map(|(l, n)| (l.to_string(), *n)).collect(),
        git: days.is_some() || commits > 0,
        commits,
        days,
        dirty,
        activity: (act_seed > 0).then(|| synthetic_activity(act_seed, commits)),
    }
}

/// Deterministic synthetic commit activity for the demo scene: a weekday-daytime weighted
/// pattern with occasional spikes, scaled to the project's commit count.
fn synthetic_activity(seed: u64, commits: u64) -> Activity {
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let weights: Vec<f64> = (0..364)
        .map(|i| {
            let day = 363 - i; // 0 = today
            let weekday = ((day + 3) % 7) as u64; // 0 = Monday, matching per_day ordering
            let weekend = if weekday >= 5 { 0.15 } else { 1.0 };
            let recency = 0.5 + 0.5 * (day as f64 / 363.0);
            (next() % 100) as f64 / 100.0 * weekend * recency
        })
        .collect();
    let total_w: f64 = weights.iter().sum();
    let per_day: Vec<u32> = weights
        .iter()
        .map(|&w| ((w / total_w) * commits as f64) as u32)
        .collect();
    // Distribute commits over hours with a 9-to-17 daytime preference.
    let mut by_hour = vec![0u32; 168];
    for (day, &n) in per_day.iter().enumerate() {
        if n == 0 {
            continue;
        }
        let weekday = ((363 - day) + 3) % 7;
        for _ in 0..n {
            let r = (next() % 100) as usize;
            let hour = if r < 70 { 9 + (next() as usize) % 9 } else { (next() as usize) % 24 };
            by_hour[weekday * 24 + hour.min(23)] += 1;
        }
    }
    Activity { per_day, by_hour }
}

/// The sixteen biggest folders from the 13 Nov 2026 scan, values as scanned.
pub fn demo_projects() -> Vec<Project> {
    vec![
        p("chinchilla", "AI / ML", "Python, PyTorch, Modal, HuggingFace", 29_989_306_270, 54_448, 25_480, 16_997_903_231, 0, 0, 12_991_403_039, 39, Some(86), 116, &[("Python", 21_000), ("TypeScript", 4_480)], 7),
        p("ply", "Native Desktop", "Rust, GPUI, Win32", 20_180_869_831, 36_227, 22_687, 3_289_539, 0, 20_172_547_517, 5_032_775, 69, Some(17), 7, &[("Rust", 22_687)], 11),
        p("yotes", "Web App", "Next.js, React, SQLite", 5_392_191_943, 55_760, 27_652, 23_292_848, 637_392_110, 4_325_998_877, 405_508_108, 18, Some(0), 53, &[("TypeScript", 27_652)], 23),
        p("messenger", "Mobile", "Expo, Convex, SQLite, Kotlin", 2_588_247_010, 41_909, 39_597, 713_040_710, 1_206_035_976, 0, 669_170_324, 71, Some(10), 5, &[("TypeScript", 27_597), ("Kotlin", 12_000)], 29),
        p("messenger-relay-next", "Mobile", "Expo, Convex, SQLite, WebRTC", 2_117_295_322, 38_558, 39_421, 323_966_571, 1_125_190_671, 0, 668_138_080, 73, Some(9), 0, &[("TypeScript", 39_421)], 31),
        p("ply-research", "Native Desktop", "Rust, GPUI", 2_049_152_253, 3_473, 20_164, 3_250_494, 0, 2_045_901_759, 0, 49, Some(23), 5, &[("Rust", 20_164)], 37),
        p("messenger-now", "Mobile", "Expo, Convex, TS", 1_835_757_152, 36_576, 36_464, 197_914_108, 1_008_872_288, 0, 628_970_756, 58, Some(9), 0, &[("TypeScript", 36_464)], 41),
        p("camx2", "AI / ML", "Next.js, ONNX, WebGPU, LM Studio", 1_357_629_566, 1_294, 41_413, 643_199_605, 0, 0, 714_429_961, 90, Some(151), 1, &[("TypeScript", 41_413)], 43),
        p("raycast-dictation-clone", "Native Desktop", "Rust, GPUI, sherpa-onnx", 1_280_782_372, 545, 14_312, 1_247_188_309, 0, 0, 33_594_063, 64, Some(60), 11, &[("Rust", 14_312)], 47),
        p("llama.cpp", "Clone / Fork", "C, C++, ggml, CMake", 615_538_240, 4_759, 719_112, 170_210_228, 0, 0, 445_328_012, 9_606, Some(152), 2, &[("C++", 560_000), ("C", 90_000), ("C/C++ headers", 69_112)], 53),
        p("eves-eye", "AI / ML", "Next.js, Cerebras, SQLite", 562_948_358, 4_961, 16_167, 378_158_816, 0, 0, 184_789_542, 37, Some(136), 4, &[("TypeScript", 16_167)], 59),
        p("whirl", "Web App", "Next.js, Convex, Bun", 477_918_394, 4_589, 114_056, 9_313_391, 27_974_764, 0, 440_630_239, 23, Some(38), 1, &[("TypeScript", 114_056)], 61),
        p("notes", "Web App", "Next.js, MDX, Biome", 210_901_503, 1_181, 23_627, 1_707_414, 0, 0, 209_194_089, 40, Some(173), 216, &[("TypeScript", 23_627)], 67),
        p("clai", "AI Tooling", "TypeScript, Ink, AI SDK, LSP", 209_069_540, 2_951, 37_312, 74_913_548, 0, 0, 134_155_992, 54, Some(97), 1, &[("TypeScript", 37_312)], 71),
        p("winamp-player", "Native Desktop", "Rust, GPUI, rodio", 9_489_804, 3_651, 9_454, 1_006_009, 0, 0, 8_483_795, 139, Some(30), 4, &[("Rust", 9_454)], 73),
        p("sentinel", "AI Tooling", "Rust, GPUI, CDP", 737_329, 44, 8_936, 737_329, 0, 0, 0, 0, None, 0, &[("Rust", 8_936)], 0),
    ]
}
