-- Thin wrapper over the global functions exported by VoiceChat.dll.
-- Every call is protected: if the DLL is missing or older, the UI says so
-- instead of throwing every frame.

local api = { available = false, error = nil }

local function call(name, ...)
    local args = { ... }
    local ok, result = pcall(function()
        return Game[name]((table.unpack or unpack)(args))
    end)
    if ok then
        api.available = true
        return result
    end
    api.error = tostring(result)
    return nil
end

local function split(line)
    local out = {}
    for field in (line .. "\t"):gmatch("([^\t]*)\t") do
        out[#out + 1] = field
    end
    return out
end

function api.probe()
    local v = call("VoiceChat_Version")
    api.available = v ~= nil
    api.version = v
    return api.available
end

function api.status()
    local s = {}
    for _, line in ipairs(call("VoiceChat_Status") or {}) do
        local k, v = line:match("^([^=]+)=(.*)$")
        if k then
            s[k] = v
        end
    end
    return s
end

function api.get(key)
    return call("VoiceChat_Get", key) or ""
end

-- Returns nil on success, else the error text.
function api.set(key, value)
    local err = call("VoiceChat_Set", key, tostring(value))
    if err == nil or err == "" then
        return nil
    end
    return err
end

function api.devices(input)
    return call("VoiceChat_Devices", input) or {}
end

function api.rescan()
    call("VoiceChat_RescanDevices")
end

function api.meters()
    local m = call("VoiceChat_Meters") or {}
    return {
        input = m[1] or -100,
        processed = m[2] or -100,
        gate = (m[3] or 0) > 0.5,
        transmitting = (m[4] or 0) > 0.5,
        threshold = m[5] or -45,
    }
end

function api.players()
    local list = {}
    for _, line in ipairs(call("VoiceChat_Players") or {}) do
        local f = split(line)
        list[#list + 1] = {
            id = tonumber(f[1]),
            name = f[2] or "?",
            party = f[3] or "",
            talking = f[4] == "1",
            volume = tonumber(f[5]) or 1,
            muted = f[6] == "1",
            level = tonumber(f[7]) or -100,
            me = f[8] == "1",
        }
    end
    return list
end

function api.speakers()
    local list = {}
    for _, line in ipairs(call("VoiceChat_Speakers") or {}) do
        local f = split(line)
        list[#list + 1] = {
            id = tonumber(f[1]),
            pos = { x = tonumber(f[2]), y = tonumber(f[3]), z = tonumber(f[4]) },
            party = f[5] == "1",
            ear = tonumber(f[6]),
        }
    end
    return list
end

function api.setPath(id, from, gain, cutoff)
    call("VoiceChat_SetPath", id, from.x, from.y, from.z, gain, cutoff)
end

-- A second way in, from another side (gain 0: none).
function api.setPathAlt(id, from, gain, cutoff)
    call("VoiceChat_SetPathAlt", id, from.x, from.y, from.z, gain, cutoff)
end

-- Our ears above our feet (m).
function api.setEar(m)
    call("VoiceChat_SetEar", m)
end

-- What only this player hears now ("radio", "call", "menu"), "" for nothing.
function api.setPersonal(reason)
    call("VoiceChat_SetPersonal", reason)
end

function api.setSurroundings(wet, size, damp)
    call("VoiceChat_SetSurroundings", wet, size, damp)
end

-- Into the VoiceChat log (red4ext\\logs\\voicechat-*.log).
function api.log(msg)
    call("VoiceChat_Log", tostring(msg))
end

-- Into the diagnostics register (scheda Diagnostica), with a hint chosen by
-- `code` in the DLL.
function api.event(level, phase, code, detail)
    call("VoiceChat_Event", level, phase, code, tostring(detail))
end

function api.resolveEvent(code)
    call("VoiceChat_ResolveEvent", code)
end

function api.events()
    local list = {}
    for _, line in ipairs(call("VoiceChat_Events") or {}) do
        local f = split(line)
        list[#list + 1] = {
            time = f[1] or "",
            level = f[2] or "info",
            phase = f[3] or "",
            detail = f[4] or "",
            hint = f[5] or "",
            count = tonumber(f[6]) or 1,
        }
    end
    return list
end

function api.clearEvents()
    call("VoiceChat_ClearEvents")
end

function api.captureKey()
    call("VoiceChat_CaptureKey")
end

function api.capturedKey()
    return call("VoiceChat_CapturedKey") or 0
end

-- A random room code for manual mode (the DLL draws it).
function api.newRoomCode()
    return call("VoiceChat_NewRoomCode") or ""
end

function api.keyName(vk)
    return call("VoiceChat_KeyName", vk) or tostring(vk)
end

return api
