-- Sounds only this player hears: the pocket radio, phone calls, menus (the
-- world is muted or ducked under them). While one plays, the DLL stops
-- reading the place from the game's sound and keeps its last reading.
--
-- The checks are the game's script names (pocket radio IsActive, the
-- player's IsPhoneCallActive), plus the UI blackboards for the hub menus and
-- the phone's call state; `checks` remembers which method answered.

local api = require("modules/api")

local P = { reason = "", checks = {} }

local EVERY = 0.25
-- Keep holding a moment after it stops: the radio's or the call's last second.
local TAIL = 1.0
local timer, quietFor = 0, 0

-- First method of `obj` among `names` that answers; remembers which one.
local function ask(key, obj, names)
    if not obj then
        return nil
    end
    local known = P.checks[key]
    for _, name in ipairs(known and known ~= "" and { known } or names) do
        local ok, v = pcall(function()
            return obj[name](obj)
        end)
        if ok and type(v) == "boolean" then
            P.checks[key] = name
            return v
        end
    end
    P.checks[key] = ""
    return nil
end

local function inMenu()
    local ok, v = pcall(function()
        local defs = GetAllBlackboardDefs()
        return Game.GetBlackboardSystem():Get(defs.UI_System):GetBool(defs.UI_System.IsInMenu)
    end)
    if ok and type(v) == "boolean" then
        P.checks.hub = "UI_System.IsInMenu"
        return v
    end
    P.checks.hub = ""
    return false
end

local function menu()
    local ok, h = pcall(function()
        return Game.GetSystemRequestsHandler()
    end)
    -- The pause menu pauses the game; the hub (inventory, map, journal...) is a menu.
    return ok and (ask("menu", h, { "IsGamePaused" }) or ask("pregame", h, { "IsPreGame" })) or inMenu()
end

local function radio(player)
    -- In a car the pocket radio reads active for the car's radio, which is in the world.
    local inCar, car = pcall(function()
        return Game.GetMountedVehicle(player)
    end)
    if inCar and car then
        return false
    end
    local ok, r = pcall(function()
        return player:GetPocketRadio()
    end)
    return ok and ask("radio", r, { "IsActive" }) or false
end

-- The phone HUD's call state: ringing or talking (questPhoneCallPhase
-- IncomingCall = 1, StartCall = 2; EndCall = 3 stays after it ends).
local function phoneHud()
    local ok, phase = pcall(function()
        local defs = GetAllBlackboardDefs()
        local v = Game.GetBlackboardSystem():Get(defs.UI_ComDevice):GetVariant(defs.UI_ComDevice.PhoneCallInformation)
        local info = FromVariant(v)
        return info and info.callPhase
    end)
    if not ok or phase == nil then
        P.checks.phone = ""
        return false
    end
    P.checks.phone = "UI_ComDevice.PhoneCallInformation"
    local okInt, n = pcall(EnumInt, phase)
    if okInt and type(n) == "number" then
        return n == 1 or n == 2
    end
    local name = tostring(phase)
    return name:find("IncomingCall") ~= nil or name:find("StartCall") ~= nil
end

local function call(player)
    return ask("call", player, { "IsPhoneCallActive" }) or phoneHud()
end

function P.update(dt)
    timer = timer + dt
    if timer < EVERY then
        return
    end
    local step = timer
    timer = 0
    local player = Game.GetPlayer()
    local now = ""
    if menu() then
        now = "menu"
    elseif player and radio(player) then
        now = "radio"
    elseif player and call(player) then
        now = "call"
    end
    if now ~= "" then
        P.reason, quietFor = now, 0
    elseif P.reason ~= "" then
        quietFor = quietFor + step
        if quietFor >= TAIL then
            P.reason = ""
        end
    end
    api.setPersonal(P.reason)
end

return P
