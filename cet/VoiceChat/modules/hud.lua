-- Voice indicator in the game's HUD, built from the game's own ink widgets,
-- font (Rajdhani) and palette, so it sits with the rest of the interface:
--
--   ▌[mic] ▮▮▮▮▮▮▮▮  PTT · MOUSE4
--
-- With the chat muted the microphone becomes crossed-out headphones.
-- It is attached to the crosshair container when the HUD starts, so it hides
-- with the HUD (menus, scenes, photo mode).

local api = require("modules/api")
local tr = require("modules/i18n").tr

local H = {}

-- The game's HUD palette (MainColors.Red / MainColors.Blue).
local RED = { 1.1761, 0.3809, 0.3476 }
local CYAN = { 0.3686, 0.9647, 1.0 }
local GREY = { 0.45, 0.45, 0.47 }
local DARK = { 0.0, 0.0, 0.0 }
local FONT = "base\\gameplay\\gui\\fonts\\raj\\raj.inkfontfamily"

local BARS = 8

local ui = nil
-- Root size / 1920: settings are in 1920x1080 pixels whatever the HUD uses.
local unit = 2
local reportedError = nil

local function color(c, a)
    return HDRColor.new({ Red = c[1], Green = c[2], Blue = c[3], Alpha = a or 1 })
end

local function place(w, parent, x, y, wdt, hgt)
    w:SetAnchor(inkEAnchor.TopLeft)
    w:SetMargin(inkMargin.new({ left = x, top = y, right = 0, bottom = 0 }))
    if wdt then
        w:SetSize(Vector2.new({ X = wdt, Y = hgt }))
    end
    w:Reparent(parent, -1)
    return w
end

local function rect(parent, x, y, w, h, c, a)
    local r = place(inkRectangle.new(), parent, x, y, w, h)
    r:SetTintColor(color(c))
    r:SetOpacity(a or 1)
    return r
end

local function text(parent, x, y, size, style)
    local t = place(inkText.new(), parent, x, y, 700, size + 12)
    pcall(function()
        t:SetFitToContent(true)
    end)
    t:SetFontFamily(FONT)
    t:SetFontStyle(CName.new(style or "Medium"))
    t:SetFontSize(size)
    t:SetLetterCase(textLetterCase.UpperCase)
    t:SetTintColor(color(RED))
    return t
end

local function cname(c)
    local ok, v = pcall(function()
        return c.value
    end)
    return ok and v and tostring(v) or tostring(c)
end

local function describe(w)
    local ok, d = pcall(function()
        local size = w:GetSize()
        return string.format("%s '%s' %.0fx%.0f", cname(w:GetClassName()), cname(w:GetName()), size.X, size.Y)
    end)
    return ok and d or ("? (" .. tostring(d) .. ")")
end

local function build(root, x, y)
    local box = inkCanvas.new()
    box:SetName(CName.new("VoiceChatHud"))
    place(box, root, x, y, 600, 300)

    local panel = {}
    panel.bg = rect(box, 0, 0, 470, 70, DARK, 0.35)
    panel.accent = rect(box, 0, 0, 5, 70, RED)
    panel.topline = rect(box, 5, 0, 440, 2, RED, 0.6)
    -- Chamfered corner, like the game's panels.
    local corner = rect(box, 448, 9, 30, 2, RED, 0.6)
    corner:SetRotation(45)

    -- Microphone glyph: capsule, cradle, stem and base.
    local mic = {}
    mic[#mic + 1] = rect(box, 26, 10, 14, 26, RED)
    mic[#mic + 1] = rect(box, 20, 26, 3, 14, RED)
    mic[#mic + 1] = rect(box, 43, 26, 3, 14, RED)
    mic[#mic + 1] = rect(box, 20, 40, 26, 3, RED)
    mic[#mic + 1] = rect(box, 31, 43, 4, 9, RED)
    mic[#mic + 1] = rect(box, 24, 52, 18, 3, RED)
    -- Headphones, shown instead when the chat is muted: band, arms, cups.
    local phones = {}
    phones[#phones + 1] = rect(box, 21, 10, 24, 3, RED)
    phones[#phones + 1] = rect(box, 18, 12, 3, 16, RED)
    phones[#phones + 1] = rect(box, 45, 12, 3, 16, RED)
    phones[#phones + 1] = rect(box, 15, 28, 8, 16, RED)
    phones[#phones + 1] = rect(box, 43, 28, 8, 16, RED)
    local slash = rect(box, 33, 4, 3, 62, RED)
    slash:SetRotation(-40)

    local bars = {}
    for i = 1, BARS do
        local h = 10 + (i - 1) * 4
        bars[i] = rect(box, 66 + (i - 1) * 10, 55 - h, 6, h, RED, 0.2)
    end

    local label = text(box, 160, 14, 34, "Semi-Bold")

    return { box = box, panel = panel, mic = mic, phones = phones, slash = slash, bars = bars, label = label }
end

-- The indicator lives in the crosshair container: full screen and always
-- drawn with the HUD. (The health bar's window and panel clip it away.)
local function remove()
    if ui then
        pcall(function()
            ui.box:SetVisible(false)
            ui.root:RemoveChild(ui.box)
        end)
        ui = nil
    end
end

local function rebuild(controller, settings)
    remove()
    local ok, err = pcall(function()
        local root = controller:GetRootCompoundWidget()
        if not root then
            error("no root widget")
        end
        local size = root:GetSize()
        unit = (size and size.X and size.X > 100) and size.X / 1920 or 2
        local s = settings or {}
        ui = build(root, (tonumber(s.hud_x) or 1577) * unit, (tonumber(s.hud_y) or 996) * unit)
        ui.root = root
        api.log(string.format("hud: attached to %s, unit %.2f", describe(root), unit))
        api.resolveEvent("hud")
    end)
    if not ok then
        H.error = tostring(err)
        api.event("warn", "HUD", "hud", tr("icona non agganciata: ", "icon not attached: ") .. H.error)
    end
    H.attached = ui ~= nil
end

-- Called whenever the game starts the crosshair container (it is recreated
-- on loads).
function H.attach(controller, settings)
    H.hooked = true
    rebuild(controller, settings)
end

-- Makes the indicator big and opaque for a few seconds, to find it.
-- (Updates run 20 times a second.)
function H.flash()
    H.flashTicks = 80
end

-- `state`: settings, meters, status, players (from the DLL).
function H.update(state)
    if not ui then
        return
    end
    local ok, err = pcall(function()
        local s = state.settings
        local visible = s.hud ~= "off"
        ui.box:SetVisible(visible)
        if not visible then
            return
        end
        local flashing = (H.flashTicks or 0) > 0
        if flashing then
            H.flashTicks = H.flashTicks - 1
        end
        local scale = (tonumber(s.hud_scale) or 1) * unit / 2 * (flashing and 2 or 1)
        ui.box:SetScale(Vector2.new({ X = scale, Y = scale }))
        local x, y = (tonumber(s.hud_x) or 1577) * unit, (tonumber(s.hud_y) or 996) * unit
        if flashing then
            -- Centre of the screen, so a clipped or shifted container shows.
            x, y = 800 * unit, 450 * unit
        end
        ui.box:SetMargin(inkMargin.new({ left = x, top = y, right = 0, bottom = 0 }))

        local m = state.meters
        local online = state.status.link == "joined"
        local deaf = s.deafened == "on"
        local muted = deaf or s.mic_muted == "on"
        local tx = m.transmitting
        local c = (not online or muted) and GREY or (tx and CYAN or RED)
        local function show(list, visible)
            for _, w in ipairs(list) do
                w:SetVisible(visible)
                w:SetTintColor(color(c))
            end
        end
        show(ui.mic, not deaf)
        show(ui.phones, deaf)
        ui.panel.accent:SetTintColor(color(c))
        ui.panel.topline:SetTintColor(color(c))
        ui.slash:SetVisible(muted)
        ui.slash:SetTintColor(color(GREY))

        local level = math.max(0, math.min(1, (m.processed + 60) / 50))
        for i, b in ipairs(ui.bars) do
            local lit = not muted and (tx or m.gate) and level >= (i - 0.5) / BARS
            b:SetTintColor(color(c))
            b:SetOpacity(lit and 1 or 0.2)
        end

        local label
        if not online then
            label = state.status.link == "connecting" and tr("Connessione...", "Connecting...")
                or tr("Offline", "Offline")
        elseif deaf then
            label = tr("Chat silenziata", "Chat muted")
        elseif muted then
            label = tr("Microfono off", "Mic off")
        elseif s.mode == "open" then
            label = tx and tr("In onda", "On air") or tr("Voce aperta", "Open mic")
        else
            label = (tx and tr("In onda · ", "On air · ") or "PTT · ") .. (s.ptt or "")
        end
        ui.label:SetText(label)
        ui.label:SetTintColor(color(c))

        local talking = false
        for _, p in ipairs(state.players) do
            if p.talking and not p.me then
                talking = true
            end
        end

        -- Quiet when nothing happens, full when someone talks.
        local busy = tx or talking
        local opacity = tonumber(s.hud_opacity) or 1
        ui.box:SetOpacity(flashing and 1 or opacity * (busy and 1 or 0.55))
    end)
    if not ok then
        H.error = tostring(err)
        if H.error ~= reportedError then
            reportedError = H.error
            api.event("warn", "HUD", "hud", tr("icona non aggiornata: ", "icon not updated: ") .. H.error)
        end
    end
end

-- On "Reload all mods" too: a widget left behind would stay frozen on screen.
function H.detach()
    remove()
    H.attached = false
end

return H
