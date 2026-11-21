//! Acoustic materials, from the name of the physics material the game gives
//! each surface ("concrete.physmat", "glass.physmat", ...).
//!
//! Values: absorption (low/mid/high), scattering, transmission (low/mid/high,
//! applied as gains to what goes through). Steam Audio's own presets, except
//! glass (V3: a shop window lets a faint, dull voice through; at -36 dB it
//! was lost altogether, in game 2026-11-20) and doors
//! (V4: an interior door, gaps around it, lets a muffled voice through).

use crate::sa::Material;

// Scattering 0.6: real walls, furniture and fittings spread sound around.
// At Steam Audio's 0.05 (mirror-like), and still at 0.3, the few strong
// echoes of a place sounded metallic (in game, 2026-11-20).
const fn m(absorption: [f32; 3], transmission: [f32; 3]) -> Material {
    Material { absorption, scattering: 0.6, transmission }
}

pub const GENERIC: u8 = 0;
pub const CONCRETE: u8 = 1;
pub const BRICK: u8 = 2;
pub const GLASS: u8 = 3;
pub const WOOD: u8 = 4;
pub const METAL: u8 = 5;
pub const PLASTER: u8 = 6;
pub const SOFT: u8 = 7;
pub const ROCK: u8 = 8;
pub const GROUND: u8 = 9;
pub const CERAMIC: u8 = 10;
pub const DOOR: u8 = 11;
pub const GRATE: u8 = 12;

pub static MATERIALS: [Material; 13] = [
    m([0.10, 0.20, 0.30], [0.100, 0.050, 0.030]), // generic
    m([0.05, 0.07, 0.08], [0.015, 0.002, 0.001]), // concrete
    m([0.03, 0.04, 0.07], [0.015, 0.015, 0.015]), // brick
    m([0.06, 0.03, 0.02], [0.045, 0.028, 0.010]), // glass: -27/-31/-40 dB
    m([0.11, 0.07, 0.06], [0.070, 0.014, 0.005]), // wood
    m([0.20, 0.07, 0.06], [0.200, 0.025, 0.010]), // metal
    m([0.12, 0.06, 0.04], [0.056, 0.056, 0.004]), // plaster
    m([0.24, 0.69, 0.73], [0.020, 0.005, 0.003]), // carpet, fabric, foam
    m([0.13, 0.20, 0.24], [0.015, 0.002, 0.001]), // rock, stone
    m([0.60, 0.70, 0.80], [0.031, 0.012, 0.008]), // gravel, earth, grass
    m([0.01, 0.02, 0.02], [0.060, 0.044, 0.011]), // ceramic, tiles, water
    m([0.11, 0.07, 0.06], [0.250, 0.180, 0.080]), // door: -12/-15/-22 dB
    m([0.05, 0.05, 0.05], [0.800, 0.700, 0.500]), // grate, mesh, fence: mostly holes
];

pub const NAMES: [&str; 13] =
    ["generic", "concrete", "brick", "glass", "wood", "metal", "plaster", "soft", "rock", "ground", "ceramic", "door", "grate"];

/// Our material for a game physics material name. Unknown names: generic.
pub fn classify(name: &str) -> u8 {
    let n = name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if has(&["door"]) {
        DOOR
    } else if has(&["glass", "window"]) {
        GLASS
    // Names met in game (2026-11-20): metal_semitrasparent (sic), metal_transparent, metal_catwalk.
    } else if has(&["semitrasparent", "semitransparent", "transparent", "catwalk", "mesh", "grate", "grill", "fence", "chain"]) {
        GRATE
    } else if has(&[
        "carpet", "fabric", "cloth", "cushion", "foam", "leather", "rubber", "textile", "mattress", "paper", "cardboard", "flesh", "meat",
        "upholster", "tire", "food", "trash_bag",
    ]) {
        SOFT
    } else if has(&["metal", "steel", "iron", "alu", "tin", "copper", "chrome", "vent", "chassis"]) {
        METAL
    } else if has(&["wood", "plank", "timber"]) {
        WOOD
    } else if has(&["plaster", "drywall", "gypsum", "plastic"]) {
        PLASTER
    } else if has(&["brick"]) {
        BRICK
    } else if has(&["ceramic", "porcelain", "tile", "water", "marble"]) {
        CERAMIC
    } else if has(&["rock", "stone", "granite"]) {
        ROCK
    } else if has(&["gravel", "dirt", "sand", "soil", "grass", "mud", "foliage", "plant", "tree", "leaves", "earth"]) {
        GROUND
    } else if has(&["concrete", "asphalt", "cement"]) {
        CONCRETE
    } else {
        GENERIC
    }
}

/// Share of rebounding sound a closed door lets out (gaps, thin panel):
/// about -10 dB, against nothing at all through Steam Audio's rebounds.
const DOOR_LEAK: f32 = 0.1;

/// Share of rebounding sound a grate, a mesh or a fence lets through.
const GRATE_LEAK: f32 = 0.7;

/// Share of rays a material lets through (see `sa::passes`).
pub fn leak(material: *const Material) -> f32 {
    if std::ptr::eq(material, &MATERIALS[usize::from(DOOR)]) {
        DOOR_LEAK
    } else if std::ptr::eq(material, &MATERIALS[usize::from(GRATE)]) {
        GRATE_LEAK
    } else {
        0.0
    }
}

/// Pointer Steam Audio reads a hit's material through (never written).
pub fn ptr(index: u8) -> *mut Material {
    &MATERIALS[usize::from(index).min(MATERIALS.len() - 1)] as *const Material as *mut Material
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_names_map_to_materials() {
        assert_eq!(classify("concrete.physmat"), CONCRETE);
        assert_eq!(classify("glass_thin.physmat"), GLASS);
        assert_eq!(classify("metal_door.physmat"), DOOR);
        assert_eq!(classify("Asphalt.physmat"), CONCRETE);
        assert_eq!(classify("something_new.physmat"), GENERIC);
        // As the game names them.
        assert_eq!(classify("metal_semitrasparent"), GRATE);
        assert_eq!(classify("metal_catwalk"), GRATE);
        assert_eq!(classify("glass_semitransparent"), GLASS);
        assert_eq!(classify("furniture_upholstery"), SOFT);
        assert_eq!(classify("vehicle_chassis"), METAL);
        assert_eq!(classify("tire_car"), SOFT);
    }
}
