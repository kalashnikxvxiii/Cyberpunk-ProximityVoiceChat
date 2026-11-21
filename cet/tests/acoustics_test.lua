-- Offline test of modules/acoustics.lua against a fake physics world made of
-- axis-aligned boxes. Run from the repo root: luajit cet/tests/acoustics_test.lua

package.path = "cet/VoiceChat/?.lua;" .. package.path

local walls = {}
local paths = {}
local speakers = {}
local surroundings = nil

package.loaded["modules/api"] = {
    speakers = function()
        return speakers
    end,
    setPath = function(id, from, gain, cutoff)
        paths[id] = { from = from, gain = gain, cutoff = cutoff }
    end,
    setPathAlt = function(id, from, gain, cutoff)
        alts[id] = { from = from, gain = gain, cutoff = cutoff }
    end,
    setSurroundings = function(wet, size, damp)
        surroundings = { wet = wet, size = size, damp = damp }
    end,
    setEar = function(m)
        sentEar = m
    end,
}

-- Segment vs box (slab method): distance along a->b of the first entry.
local function segBox(a, b, box)
    local tmin, tmax = 0, 1
    for _, ax in ipairs({ "x", "y", "z" }) do
        local d = b[ax] - a[ax]
        local lo, hi = box.min[ax], box.max[ax]
        if math.abs(d) < 1e-9 then
            if a[ax] < lo or a[ax] > hi then
                return nil
            end
        else
            local t1, t2 = (lo - a[ax]) / d, (hi - a[ax]) / d
            if t1 > t2 then
                t1, t2 = t2, t1
            end
            tmin, tmax = math.max(tmin, t1), math.min(tmax, t2)
            if tmin > tmax then
                return nil
            end
        end
    end
    return tmin
end

local sqs = {
    SyncRaycastByCollisionGroup = function(_, a, b, g)
        local best = nil
        for _, w in ipairs(walls) do
            local t = (w.group or "Static") == g and segBox(a, b, w)
            -- A start inside a box stops the ray there, as PhysX does.
            if t and (not best or t < best) then
                best = t
            end
        end
        if not best then
            return false, { position = { x = 0, y = 0, z = 0 } }
        end
        return true,
            { position = { x = a.x + (b.x - a.x) * best, y = a.y + (b.y - a.y) * best, z = a.z + (b.z - a.z) * best } }
    end,
}

local playerPos = { x = 0, y = 0, z = 0 }
-- The first-person camera's world position; nil: it can't be read.
local camAt = nil
Vector4 = {
    new = function(x, y, z)
        return { x = x, y = y, z = z }
    end,
}
CName = {
    new = function(s)
        return s
    end,
}
Game = {
    GetPlayer = function()
        return {
            GetWorldPosition = function()
                return playerPos
            end,
            GetFPPCameraComponent = function()
                return {
                    GetLocalToWorld = function()
                        return { W = assert(camAt, "no camera") }
                    end,
                }
            end,
        }
    end,
    GetSpatialQueriesSystem = function()
        return sqs
    end,
}

local acoustics = require("modules/acoustics")
local settings = { occlusion = "on", reverb = "on" }

local function box(x1, y1, z1, x2, y2, z2)
    return { min = { x = x1, y = y1, z = z1 }, max = { x = x2, y = y2, z = z2 } }
end

local function run(name, setup, check)
    walls, paths, speakers, alts = {}, {}, {}, {}
    acoustics.reset()
    setup()
    for _ = 1, 3 do
        acoustics.update(0.3, settings)
    end
    local p = paths[1]
    local ok, msg = check(p)
    print(
        (ok and "ok   " or "FAIL ")
            .. name
            .. "  -> "
            .. string.format("gain %.2f cutoff %.0f", p and p.gain or -1, p and p.cutoff or -1)
            .. (msg and ("  " .. msg) or "")
    )
    if acoustics.error then
        print("     error: " .. acoustics.error)
    end
    return ok
end

local all = true
local function test(...)
    all = run(...) and all
end

-- Speaker 6 m away on the X axis.
local function speakerAt(x, y)
    speakers = { { id = 1, pos = { x = x, y = y, z = 0 }, party = false } }
end

test("open field", function()
    speakerAt(6, 0)
end, function(p)
    return p.gain > 0.99 and p.cutoff > 17000
end)

test("thick wall, no openings: silent", function()
    speakerAt(6, 0)
    walls = { box(2.5, -30, -1, 3.5, 30, 10) } -- 1 m concrete, 60 m long
end, function(p)
    return p.gain == 0
end)

test("thin wall, no openings: muffled", function()
    speakerAt(6, 0)
    walls = { box(2.9, -30, -1, 3.1, 30, 10) } -- 20 cm partition
end, function(p)
    return p.gain > 0 and p.gain < 0.3 and p.cutoff <= 600
end)

test("thick wall with a door 3 m to the side: heard through the door", function()
    speakerAt(6, 0)
    -- Wall at x=3 with a 1.2 m gap at y in [2.4, 3.6].
    walls = { box(2.5, -30, -1, 3.5, 2.4, 10), box(2.5, 3.6, -1, 3.5, 30, 10) }
end, function(p)
    local fromDoorSide = p.from.y > 0.5
    return p.gain > 0.1 and p.gain < 0.95 and p.cutoff < 17000 and fromDoorSide,
        string.format("from (%.1f, %.1f)", p.from.x, p.from.y)
end)

test("same door, closed: much fainter and dark, still from the door", function()
    speakerAt(6, 0)
    walls = { box(2.5, -30, -1, 3.5, 2.4, 10), box(2.5, 3.6, -1, 3.5, 30, 10) }
    -- A door in the gap: blocks the player and moves (two groups, same place).
    for _, g in ipairs({ "PlayerBlocker", "Dynamic" }) do
        local d = box(2.95, 2.4, -1, 3.05, 3.6, 2.2)
        d.group = g
        walls[#walls + 1] = d
    end
end, function(p)
    return p.gain > 0 and p.gain < 0.1 and p.cutoff <= 1000 and p.from.y > 0.5,
        string.format("from (%.1f, %.1f)", p.from.x, p.from.y)
end)

test("standing against a closed door (inside its player blocker): still closed", function()
    speakerAt(6, 0)
    walls = { box(0.25, -30, -1, 0.35, -0.6, 10), box(0.25, 0.6, -1, 0.35, 30, 10) }
    local d = box(0.28, -0.6, -1, 0.32, 0.6, 2.2)
    d.group = "Dynamic"
    walls[#walls + 1] = d
    -- The blocker keeps the player off the door: we stand in it.
    local b = box(-0.3, -0.6, -1, 0.9, 0.6, 2.2)
    b.group = "PlayerBlocker"
    walls[#walls + 1] = b
end, function(p)
    return p.gain > 0 and p.gain < 0.3 and p.cutoff <= 1000
end)

test("closed door to the side, its player blocker thick: still closed around the corner", function()
    speakerAt(6, 0)
    walls = { box(2.5, -30, -1, 3.5, 2.4, 10), box(2.5, 3.6, -1, 3.5, 30, 10) }
    local d = box(2.95, 2.4, -1, 3.05, 3.6, 2.2)
    d.group = "Dynamic"
    walls[#walls + 1] = d
    -- Keeps the player a metre off the door on both sides (V4).
    local b = box(1.9, 2.4, -1, 4.1, 3.6, 2.2)
    b.group = "PlayerBlocker"
    walls[#walls + 1] = b
end, function(p)
    return p.gain > 0 and p.gain < 0.1 and p.cutoff <= 1000
end)

test("closed door with a seam the line slips through: still closed", function()
    speakerAt(6, 0)
    walls = { box(2.5, -30, -1, 3.5, -0.6, 10), box(2.5, 0.6, -1, 3.5, 30, 10) }
    -- Two panels with a 2 cm gap right on our line (V4).
    for _, g in ipairs({ "PlayerBlocker", "Dynamic" }) do
        for _, y in ipairs({ { -0.6, -0.01 }, { 0.01, 0.6 } }) do
            local d = box(2.95, y[1], -1, 3.05, y[2], 2.2)
            d.group = g
            walls[#walls + 1] = d
        end
    end
end, function(p)
    return p.gain > 0 and p.gain < 0.3 and p.cutoff <= 1000
end)

test("seam in a thick wall: not a way through", function()
    speakerAt(6, 0)
    walls = { box(2.5, -30, -1, 3.5, -0.01, 10), box(2.5, 0.01, -1, 3.5, 30, 10) }
end, function(p)
    return p.gain == 0
end)

test("parapet higher than our heads: over the top, barely bent", function()
    speakerAt(6, 0)
    walls = { box(2.9, -30, -1, 3.1, 30, 2.0) } -- 20 cm thick, top at 2 m, heads at 1.6
end, function(p)
    local dy, dx = math.abs(p.from.y), p.from.x
    return p.gain > 0.3 and dy < 0.5 and dx > 3, string.format("from (%.1f, %.1f, %.1f)", p.from.x, p.from.y, p.from.z)
end)

test("two rooms, doors in different places: two bends", function()
    speakerAt(12, 0)
    -- Our room x<3 (door at y=4), corridor 3..9, their room x>9 (door at y=-4).
    walls = {
        box(2.5, -30, -1, 3.5, 3.4, 10),
        box(2.5, 4.6, -1, 3.5, 30, 10),
        box(8.5, -30, -1, 9.5, -4.6, 10),
        box(8.5, -3.4, -1, 9.5, 30, 10),
    }
end, function(p)
    return p.gain > 0.02 and p.gain < 0.9, string.format("from (%.1f, %.1f)", p.from.x, p.from.y)
end)

test("short wall: around the corner", function()
    speakerAt(6, 0)
    walls = { box(2.5, -3, -1, 3.5, 3, 10) } -- 6 m long free-standing wall
end, function(p)
    return p.gain > 0.2 and p.gain < 0.99, string.format("from (%.1f, %.1f)", p.from.x, p.from.y)
end)

test("door 20 m away along a thick wall: too far, silent", function()
    speakerAt(6, 0)
    walls = { box(2.5, -40, -1, 3.5, 19.4, 10), box(2.5, 20.6, -1, 3.5, 40, 10) }
end, function(p)
    return p.gain == 0
end)

-- A car around the listener: 4.5 x 1.8 x 1.4 m, roof below head height.
local function car(x)
    local c = box(x - 2.25, -0.9, 0.2, x + 2.25, 0.9, 1.4)
    c.group = "Vehicle"
    return c
end

test("listener in a car, speaker outside: muffled", function()
    speakerAt(6, 0)
    walls = { car(0) }
end, function(p)
    return p.gain > 0.1 and p.gain < 0.5 and p.cutoff <= 1000
end)

test("speaker in another car: muffled", function()
    speakerAt(8, 0)
    walls = { car(0), car(8) }
end, function(p)
    return p.gain > 0.1 and p.gain < 0.5 and p.cutoff <= 1000
end)

test("both in the same car: clear", function()
    speakerAt(0.8, 0.5)
    walls = { car(0) }
end, function(p)
    return p.gain > 0.99 and p.cutoff > 17000
end)

test("shop window between us: heard, but faint and dark", function()
    speakerAt(6, 0)
    local pane = box(2.99, -30, -1, 3.01, 30, 10) -- 2 cm glass
    pane.group = "Destructible"
    walls = { pane }
end, function(p)
    return p.gain < 0.1 and p.gain > 0 and p.cutoff <= 900
end)

test("shop window with the door open beside it: heard through both", function()
    speakerAt(6, 0)
    -- Shop front at x=3: glass in front of us, an open door at y in [3, 4.2].
    local pane = box(2.99, -30, -1, 3.01, 3, 10)
    pane.group = "Destructible"
    walls = { pane, box(2.5, 4.2, -1, 3.5, 30, 10) }
end, function(p)
    local a = alts[1]
    local ok = a and a.gain > 0 and p.gain > 0
    local fromDoor = (p.from.y > 1.5) or (a and a.from.y > 1.5)
    local fromGlass = (math.abs(p.from.y) < 0.6) or (a and math.abs(a.from.y) < 0.6)
    return ok and fromDoor and fromGlass,
        string.format("main (%.1f, %.1f) %.3f; alt %s", p.from.x, p.from.y, p.gain,
            a and string.format("(%.1f, %.1f) %.3f", a.from.x, a.from.y, a.gain) or "none")
end)

test("a crate in the way is not a window", function()
    speakerAt(6, 0)
    local crate = box(2.5, -0.5, -1, 3.5, 0.5, 3)
    crate.group = "Destructible"
    walls = { crate }
end, function(p)
    return p.gain > 0.99
end)

-- Surroundings: tunnel vs open air.
walls, speakers = {}, {}
acoustics.reset()
for _ = 1, 10 do
    acoustics.update(0.3, settings)
end
local open = surroundings.wet
walls = { box(-200, -4, -1, 200, -3, 10), box(-200, 3, -1, 200, 4, 10), box(-200, -4, 4, 200, 4, 5) }
acoustics.reset()
for _ = 1, 20 do
    acoustics.update(0.3, settings)
end
local tunnel = surroundings.wet
local tunnelSize = surroundings.size
-- Street canyon: tall buildings both sides, open sky, a sign overhead.
walls = { box(-200, -8, -1, 200, -7, 60), box(-200, 7, -1, 200, 8, 60), box(-1, -1, 9, 1, 1, 9.3) }
acoustics.reset()
for _ = 1, 20 do
    acoustics.update(0.3, settings)
end
local street = surroundings.wet
-- Wide avenue: buildings 15 m away, 30 m tall.
walls = { box(-200, -16, -1, 200, -15, 30), box(-200, 15, -1, 200, 16, 30) }
acoustics.reset()
for _ = 1, 20 do
    acoustics.update(0.3, settings)
end
local avenue = surroundings.wet
-- Concrete hall 20x20x8.
walls = {
    box(-11, -11, -1, 11, -10, 8),
    box(-11, 10, -1, 11, 11, 8),
    box(-11, -11, -1, -10, 11, 8),
    box(10, -11, -1, 11, 11, 8),
    box(-11, -11, 7, 11, 11, 8),
}
acoustics.reset()
for _ = 1, 20 do
    acoustics.update(0.3, settings)
end
local hall = surroundings.wet
-- Same hall, furnished: tables and shelves (low, in front of the walls).
for i = -8, 8, 4 do
    walls[#walls + 1] = box(i - 1, -9, -1, i + 1, -7, 1.0)
    walls[#walls + 1] = box(i - 1, 7, -1, i + 1, 9, 1.0)
    walls[#walls + 1] = box(-9, i - 1, -1, -7, i + 1, 1.0)
    local t = box(7, i - 1, -1, 9, i + 1, 1.0)
    t.group = "Dynamic"
    walls[#walls + 1] = t
end
acoustics.reset()
for _ = 1, 20 do
    acoustics.update(0.3, settings)
end
local furnished = surroundings.wet
print(string.format("     furnished hall %.2f", furnished))
-- Car park: 60x60 m, ceiling 3 m up, concrete pillars every 8 m.
walls = {
    box(-31, -31, -1, 31, -30, 4),
    box(-31, 30, -1, 31, 31, 4),
    box(-31, -31, -1, -30, 31, 4),
    box(30, -31, -1, 31, 31, 4),
    box(-31, -31, 3, 31, 31, 4),
}
for i = -28, 28, 8 do
    for j = -28, 28, 8 do
        walls[#walls + 1] = box(i - 0.3, j - 0.3, -1, i + 0.3, j + 0.3, 3)
    end
end
acoustics.reset()
for _ = 1, 20 do
    acoustics.update(0.3, settings)
end
local carpark = surroundings.wet
-- Pedestrian underpass: 3 m wide, ceiling 2.6 m up.
walls = { box(-200, -2.5, -1, 200, -1.5, 10), box(-200, 1.5, -1, 200, 2.5, 10), box(-200, -2.5, 2.6, 200, 2.5, 4) }
acoustics.reset()
for _ = 1, 20 do
    acoustics.update(0.3, settings)
end
local underpass = surroundings.wet
-- Courtyard: walls all around 6 m away, 15 m tall, open sky.
walls = {
    box(-7, -7, -1, 7, -6, 15),
    box(-7, 6, -1, 7, 7, 15),
    box(-7, -7, -1, -6, 7, 15),
    box(6, -7, -1, 7, 7, 15),
}
acoustics.reset()
for _ = 1, 20 do
    acoustics.update(0.3, settings)
end
local courtyard = surroundings.wet
print(string.format("     courtyard (open sky, walls around) %.2f", courtyard))
local okEnv = open < 0.02
    and avenue < 0.02
    and street < 0.05
    and courtyard < 0.1
    and tunnel > 0.2
    and carpark > tunnel
    and underpass > 0.08
    and underpass < tunnel * 0.8
    and tunnelSize > 0.15
    and hall > 0.1
    and furnished < hall * 0.4
print(
    (okEnv and "ok   " or "FAIL ")
        .. string.format(
            "reverb: open %.2f, avenue %.2f, street canyon %.2f, hall %.2f, tunnel %.2f (size %.2f), car park %.2f, underpass %.2f",
            open,
            avenue,
            street,
            hall,
            tunnel,
            tunnelSize,
            carpark,
            underpass
        )
)
all = all and okEnv

-- Ear height: the camera's when it reads sensibly, standing otherwise.
local function earAt(cam)
    camAt = cam
    acoustics.update(1, { occlusion = "off", reverb = "off" })
    return acoustics.ear, acoustics.earFrom, sentEar
end
local e1, f1, s1 = earAt(nil)
local e2, f2, s2 = earAt({ x = 0.1, y = 0, z = 1.05 })
local e3 = earAt({ x = 50, y = 0, z = 1.0 })
local okEar = e1 == 1.6 and s1 == 1.6 and math.abs(e2 - 1.05) < 1e-6 and f2 == "camera" and s2 == e2 and e3 == 1.6
print((okEar and "ok  " or "FAIL") .. string.format(" ears: none %.2f (%s), crouched camera %.2f, camera elsewhere %.2f", e1, f1, e2, e3))
all = all and okEar
camAt = nil

-- Voices Steam Audio handles in the DLL: a classic path too (the fallback).
walls, paths, speakers, alts = {}, {}, {}, {}
acoustics.reset()
speakers = { { id = 1, pos = { x = 6, y = 0, z = 0 }, party = false, how = "in sight 100%" } }
for _ = 1, 3 do
    acoustics.update(0.3, settings)
end
local okSteam = paths[1] ~= nil and paths[1].gain > 0.9
print((okSteam and "ok  " or "FAIL") .. " Steam Audio voices: classic path too")
all = all and okSteam

print(all and "ALL OK" or "SOME FAILED")
os.exit(all and 0 or 1)
