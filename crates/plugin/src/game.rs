//! Reads the local player's pose from the engine. Call only from the game thread.

use red4ext_rs::types::{GameInstance, Ref};
use red4ext_rs::{InvokeError, NativeRepr, ScriptClass, call, class_kind};

/// Opaque handle to the engine's `PlayerPuppet`; we never touch its fields.
pub struct PlayerPuppet;

unsafe impl ScriptClass for PlayerPuppet {
    type Kind = class_kind::Scripted;
    const NAME: &'static str = "PlayerPuppet";
}

/// Opaque handle to the engine's camera system.
pub struct CameraSystem;

unsafe impl ScriptClass for CameraSystem {
    type Kind = class_kind::Scripted;
    const NAME: &'static str = "gameCameraSystem";
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct Vector4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

unsafe impl NativeRepr for Vector4 {
    const NAME: &'static str = "Vector4";
}

#[derive(Debug, Clone, Copy)]
pub struct Pose {
    pub position: Vector4,
    pub forward: Vector4,
}

impl Pose {
    pub fn pos(&self) -> [f32; 3] {
        [self.position.x, self.position.y, self.position.z]
    }
}

/// Where the active camera looks. Unlike the player's body, it follows the
/// view in vehicles and when looking around in third person.
pub fn camera_forward() -> Result<Vector4, InvokeError> {
    let cam = call!("ScriptGameInstance"::"GetCameraSystem" (GameInstance::new()) -> Ref<CameraSystem>)?;
    call!(cam, "GetActiveCameraForward" () -> Vector4)
}

/// `Ok(None)` while no player exists (main menu, loading screens).
/// `forward` is the body's facing; see [`camera_forward`] for the view.
pub fn player_pose() -> Result<Option<Pose>, InvokeError> {
    let player = call!("GetPlayer;GameInstance" (GameInstance::new()) -> Ref<PlayerPuppet>)?;
    if player.is_null() {
        return Ok(None);
    }
    Ok(Some(Pose {
        position: call!(player, "GetWorldPosition" () -> Vector4)?,
        forward: call!(player, "GetWorldForward" () -> Vector4)?,
    }))
}
