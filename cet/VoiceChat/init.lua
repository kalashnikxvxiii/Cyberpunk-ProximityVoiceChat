-- VoiceChat for Cyberpunk 2077 (NightCoop): settings window, HUD indicator
-- and sound propagation. Audio, network and DSP live in VoiceChat.dll; this
-- mod talks to it through Game.VoiceChat_* functions.

local api = require("modules/api")
local tr = require("modules/i18n").tr
local acoustics = require("modules/acoustics")
local personal = require("modules/personal")
local hud = require("modules/hud")
local ui = require("modules/ui")
local wizard = require("modules/wizard")

local overlay = false
local state = {
    settings = {},
    status = {},
    meters = { input = -100, processed = -100, gate = false, transmitting = false, threshold = -45 },
    players = {},
}
local timers = { settings = 99, fast = 0, inGame = 0 }
local warnedNoHud = false

local SETTING_KEYS = {
    "language",
    "session_mode",
    "setup_done",
    "server_key",
    "server",
    "room",
    "name",
    "party",
    "input_device",
    "output_device",
    "mode",
    "ptt",
    "vad_threshold_db",
    "input_gain_db",
    "mic_volume",
    "voice_volume",
    "echo_cancel",
    "noise",
    "mic_muted",
    "deafened",
    "monitor",
    "occlusion",
    "steam_audio",
    "rebounds",
    "reverb",
    "self_reverb",
    "self_reverb_level",
    "hud",
    "hud_x",
    "hud_y",
    "hud_scale",
    "hud_opacity",
}

local function refreshSettings()
    for _, k in ipairs(SETTING_KEYS) do
        state.settings[k] = api.get(k)
    end
    require("modules/i18n").lang = state.settings.language == "it" and "it" or "en"
    state.status = api.status()
end

local function hookHud()
    -- ObserveAfter is the newer name; Observe works on older CET too.
    local observe = ObserveAfter or Observe
    observe("gameuiCrosshairContainerController", "OnInitialize", function(self)
        hud.attach(self, state.settings)
    end)
end

registerForEvent("onInit", function()
    api.probe()
    local ok, err = pcall(hookHud)
    api.log("cet mod started, DLL " .. tostring(api.version) .. (ok and "" or (", HUD hook failed: " .. tostring(err))))
end)

registerForEvent("onOverlayOpen", function()
    overlay = true
    timers.settings = 99
end)

registerForEvent("onOverlayClose", function()
    overlay = false
end)

registerForEvent("onUpdate", function(dt)
    if not api.available then
        timers.settings = timers.settings + dt
        if timers.settings > 5 then
            timers.settings = 0
            api.probe()
        end
        return
    end
    timers.settings = timers.settings + dt
    timers.fast = timers.fast + dt
    -- Settings change rarely; the UI refreshes them faster while open.
    if timers.settings >= (overlay and 0.25 or 1.0) then
        timers.settings = 0
        refreshSettings()
    end
    if timers.fast >= 0.05 then
        timers.fast = 0
        state.meters = api.meters()
        state.players = api.players()
        hud.update(state)
    end
    acoustics.update(dt, state.settings)
    personal.update(dt)
    wizard.update(dt, state)
    -- Menus (the main one too) have no crosshair: only time in the world counts.
    if not hud.hooked and not warnedNoHud and state.status.in_game == "1" and personal.reason ~= "menu" then
        timers.inGame = timers.inGame + dt
        if timers.inGame > 60 then
            warnedNoHud = true
            api.event(
                "warn",
                "HUD",
                "hud",
                tr(
                    "il contenitore del mirino non si è mai avviato da quando la mod è partita",
                    "the crosshair container never started since the mod loaded"
                )
            )
        end
    end
end)

registerForEvent("onDraw", function()
    if not overlay then
        return
    end
    local firstRun = api.available and state.settings.setup_done == "off" and not wizard.finished
    if firstRun or wizard.open then
        wizard.draw(state)
    else
        ui.draw(state, hud)
    end
end)

registerForEvent("onShutdown", function()
    hud.detach()
    acoustics.reset()
end)
