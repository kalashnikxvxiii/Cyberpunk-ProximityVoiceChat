-- How each voice reaches us through the world, using the game's physics
-- raycasts. Results go to the DLL, which muffles, attenuates and places the
-- voice accordingly.
--
-- For every speaker we look for an air path:
--   1. straight line clear            -> heard directly;
--   2. one bend: a point in our space that sees the speaker, or a point in
--      theirs that sees us (a doorway, a window, the end of a wall);
--   3. two bends: a point on our side that sees a point on theirs;
-- and take the loudest (the shortest may cross a shop window that a door
-- beside it doesn't). Bends cost volume and high frequencies, as sound
-- diffracting around an edge does, and the voice seems to come from the
-- first opening. With no air path the sound must go through the wall: thin
-- walls (doors, partitions) let a muffled voice through, thick ones nothing.
--
-- Along the air path, a thin pane in the way (a shop window) costs a lot:
-- glass lets a voice through, muffled, but nothing like an open door.
--
-- We also probe our surroundings (how enclosed we are, how far the surfaces
-- are, how much stuff stands in the room) to drive the reverb.

local api = require("modules/api")
local tr = require("modules/i18n").tr

local A = {}

local HEAD = 1.6
local PROBE_DIRS = 12
local PROBE_REACH = 9.0
local PATH_EVERY = 0.2
local ENV_EVERY = 0.25
local ENV_REACH = 40.0
-- Walls up to this thick (metres) let a muffled voice through.
local THIN_WALL = 0.25
local MEDIUM_WALL = 0.45
-- Doorway search: sideways steps along a wall, and walls crossed in a row.
local OPENING_STEP = 0.6
local OPENING_STEPS = 16
local ROUTE_DEPTH = 3
-- Diffraction: this much quieter for a path that turns right back (180°).
local BEND_DB = 18
-- What reaches the DLL follows the computed loudness over about 0.3 s, so a
-- path that switches (door found, lost) fades instead of jumping.
local SMOOTH = 0.5
local SILENT_DB = -60
local heardDb = {}

local groups = nil
local carGroups = nil
-- A car body between us (one of us inside, or two cars): windows up, so
-- quieter and darker. Checked at seated chest height, below the roof.
local CAR_GAIN = 0.3
local CAR_CUTOFF = 1000
local CAR_DROP = HEAD - 0.9
-- Glass panes on the air path: single glazing, about -26 dB, highs gone.
-- shortcut: which collision group holds shop windows is a guess, not checked in
-- game; panes in Static already count as thin walls.
local GLASS_NAMES = { "Destructible", "Glass" }
local glassGroups = nil
-- About -36 dB: a shop window lets speech through faintly (at -30 it still
-- sounded too open, V3 2026-11-20).
local GLASS_GAIN = 0.016
local GLASS_CUTOFF = 900
local GLASS_MAX_THICK = 0.12
-- Closed doors aren't Static: in game they block the player and move (they
-- show in PlayerBlocker and Dynamic at the same spot, in game 2026-11-19).
-- An interior door: about -12 dB (gaps around it let speech through), highs
-- muffled but not gone (at 700 Hz the voice lost its place in our head).
local DOOR_GROUPS = { "PlayerBlocker", "Dynamic" }
local doorGroups = nil
local DOOR_GAIN = 0.25
local DOOR_CUTOFF = 1000
-- Over the top: a parapet or low wall between us, sound bends over its edge.
local OVER_STEP = 0.3
local OVER_STEPS = 6
-- Furniture: what stands between table height and the floor.
local clutterGroups = nil
local LOW = HEAD - 0.8
local sqs = nil
local timers = { path = 0, env = 0 }
local listenerProbes = nil
local speakerProbes = {}
local env = { wet = 0, size = 0.3, damp = 0.4 }

local function v4(x, y, z)
    return Vector4.new(x, y, z, 1)
end

local function dist(a, b)
    local dx, dy, dz = a.x - b.x, a.y - b.y, a.z - b.z
    return math.sqrt(dx * dx + dy * dy + dz * dz)
end

local function add(a, b, k)
    return { x = a.x + b.x * k, y = a.y + b.y * k, z = a.z + b.z * k }
end

-- Point where the segment a->b first hits solid world, or nil if clear.
local function hit(a, b, only)
    local len = dist(a, b)
    if len < 0.05 then
        return nil
    end
    for _, g in ipairs(only or groups) do
        local ok, res = sqs:SyncRaycastByCollisionGroup(v4(a.x, a.y, a.z), v4(b.x, b.y, b.z), g, false, false)
        if ok and res and res.position then
            local p = { x = res.position.x, y = res.position.y, z = res.position.z }
            -- Ignore hits at the very ends (the floor under a probe, a
            -- wall someone leans on).
            local d = dist(a, p)
            if d > 0.05 and d < len - 0.05 and not (p.x == 0 and p.y == 0 and p.z == 0) then
                return p, d
            end
        end
    end
    return nil
end

-- The same segment moved a little sideways and up/down: a ray is a line, and
-- the game's collision has seams (a sliding door's edge, a window frame) that
-- a line slips through where no sound would.
local SEAM = 0.1
local function shifted(a, b)
    local dx, dy = b.x - a.x, b.y - a.y
    local l = math.sqrt(dx * dx + dy * dy)
    local sx, sy = 1, 0
    if l > 1e-3 then
        sx, sy = -dy / l, dx / l
    end
    local out = {}
    for _, o in ipairs({ { sx, sy, 0 }, { -sx, -sy, 0 }, { 0, 0, 1 }, { 0, 0, -1 } }) do
        local d = { x = o[1] * SEAM, y = o[2] * SEAM, z = o[3] * SEAM }
        out[#out + 1] = { add(a, d, 1), add(b, d, 1) }
    end
    return out
end

-- Clear for sound: the line and nearly all its shifted copies. A real corner
-- grazed by one copy still passes; a seam, blocked on both sides, doesn't.
local function clear(a, b)
    if hit(a, b) then
        return false
    end
    local blocked = 0
    for _, s in ipairs(shifted(a, b)) do
        if hit(s[1], s[2]) then
            blocked = blocked + 1
            if blocked >= 2 then
                return false
            end
        end
    end
    return true
end

-- Points in the same air space as `p`, around it, at head height.
local function probes(p)
    local out = {}
    for i = 0, PROBE_DIRS - 1 do
        local yaw = i * 2 * math.pi / PROBE_DIRS
        local dir = { x = math.cos(yaw), y = math.sin(yaw), z = 0 }
        local far = { x = p.x + dir.x * PROBE_REACH, y = p.y + dir.y * PROBE_REACH, z = p.z }
        -- The nearest of the line and its shifted copies: no probe past a seam.
        local _, d = hit(p, far)
        for _, sh in ipairs(shifted(p, far)) do
            local _, ds = hit(sh[1], sh[2])
            if ds and (not d or ds < d) then
                d = ds
            end
        end
        local reach = d and math.max(d - 0.4, 0) or PROBE_REACH
        for _, along in ipairs({ math.min(2.5, reach * 0.5), reach }) do
            if along > 0.8 then
                out[#out + 1] = { x = p.x + dir.x * along, y = p.y + dir.y * along, z = p.z }
            end
        end
    end
    return out
end

-- Angle (degrees) by which the path turns at b, going a -> b -> c.
local function bend(a, b, c)
    local ux, uy, uz = b.x - a.x, b.y - a.y, b.z - a.z
    local vx, vy, vz = c.x - b.x, c.y - b.y, c.z - b.z
    local lu = math.sqrt(ux * ux + uy * uy + uz * uz)
    local lv = math.sqrt(vx * vx + vy * vy + vz * vz)
    if lu < 1e-3 or lv < 1e-3 then
        return 0
    end
    local cos = (ux * vx + uy * vy + uz * vz) / (lu * lv)
    return math.deg(math.acos(math.max(-1, math.min(1, cos))))
end

local function sortedBy(list, key)
    local copy = {}
    for i, v in ipairs(list) do
        copy[i] = v
    end
    table.sort(copy, function(a, b)
        return key(a) < key(b)
    end)
    return copy
end

local function pathLength(points)
    local len = 0
    for i = 2, #points do
        len = len + dist(points[i - 1], points[i])
    end
    return len
end

-- Openings in the first wall between A and B: we step back from the wall,
-- slide sideways, and look for a straight passage through it (a door, a
-- window, a gap). Returns up to `max` pairs {before, after}, nearest first.
local function openings(A, B, max)
    local h, d = hit(A, B)
    if not h then
        return {}
    end
    local len = dist(A, B)
    local dir = { x = (B.x - A.x) / len, y = (B.y - A.y) / len, z = (B.z - A.z) / len }
    local plen = math.sqrt(dir.x * dir.x + dir.y * dir.y)
    if plen < 0.2 then
        return {}
    end
    local side = { x = -dir.y / plen, y = dir.x / plen, z = 0 }
    -- How thick the wall is here: look back at it from behind.
    local behind = add(h, dir, 4)
    local _, f = hit(behind, h)
    local thick = f and math.min(1.5, math.max(0.05, 4 - f)) or 0.5
    local back = math.max(0.2, d - 0.5)
    local through = 0.5 + thick + 0.6
    local found = {}
    for k = 1, OPENING_STEPS do
        for _, sign in ipairs({ 1, -1 }) do
            local before = add(add(A, dir, back), side, sign * k * OPENING_STEP)
            local after = add(before, dir, through)
            if clear(before, after) then
                found[#found + 1] = { before, after }
                if #found >= max then
                    return found
                end
            end
        end
    end
    return found
end

-- Air route from A to B through successive openings, or nil.
local function route(A, B, depth)
    if clear(A, B) then
        return { A, B }
    end
    if depth == 0 then
        return nil
    end
    local best, bestLen = nil, math.huge
    for _, o in ipairs(openings(A, B, 2)) do
        local before, after = o[1], o[2]
        if clear(A, before) then
            local rest = route(after, B, depth - 1)
            if rest then
                local points = { A, before }
                for _, p in ipairs(rest) do
                    points[#points + 1] = p
                end
                local len = pathLength(points)
                if len < bestLen then
                    best, bestLen = points, len
                end
            end
        end
    end
    return best
end

-- From A over the first obstacle towards B: a point just above its top edge
-- that sees both, or nil.
local function overTop(A, B)
    local h, d = hit(A, B)
    if not h then
        return nil
    end
    local len = dist(A, B)
    local back = math.max(0.1, d - 0.3) / len
    for k = 1, OVER_STEPS do
        local p = {
            x = A.x + (B.x - A.x) * back,
            y = A.y + (B.y - A.y) * back,
            z = math.max(h.z, A.z + (B.z - A.z) * back) + k * OVER_STEP,
        }
        if clear(A, p) and clear(p, B) then
            return { A, p, B }
        end
    end
    return nil
end

-- How a path sounds: its length, total turn, panes crossed, and loudness
-- relative to the voice in the open at 2 m (what candidates compete on).
local panes
local function rate(points)
    local len, turn = pathLength(points), 0
    for i = 2, #points - 1 do
        turn = turn + bend(points[i - 1], points[i], points[i + 1])
    end
    local glass, doors = panes(points)
    local gain = 10 ^ (-(turn / 180) * BEND_DB / 20) * GLASS_GAIN ^ glass * DOOR_GAIN ^ doors
    return {
        points = points,
        len = len,
        turn = turn,
        glass = glass,
        doors = doors,
        -- Through glass or a closed door: keep looking for an open way.
        shut = glass + doors,
        gain = gain,
        score = gain * 2 / math.max(len, 2),
    }
end

-- A second way in is heard too when it comes from elsewhere (this far apart
-- as seen from us) and isn't much quieter than the loudest: a voice through a
-- shop window and the open door next to it.
local SECOND_APART = 25
local SECOND_DB = 30

-- The loudest of `cands` other than `best` that comes from another side, or nil.
local function second(L, best, cands)
    local out = nil
    for _, c in ipairs(cands) do
        -- One of the two through the open air: not two panes of the same window.
        if c ~= best and (c.shut == 0 or best.shut == 0)
            and c.score * 10 ^ (SECOND_DB / 20) >= best.score
            and (not out or c.score > out.score)
            and bend(best.points[2], L, c.points[2]) < 180 - SECOND_APART then
            out = c
        end
    end
    return out
end

-- Loudest air path from listener L to speaker S (rated) and a second one
-- from another side (see `second`), or nil.
local function airPath(L, S, sp)
    local best = nil
    local cands = {}
    -- `reversed`: found from S to L.
    local function consider(points, reversed)
        if not points then
            return nil
        end
        if reversed then
            local r = {}
            for i = #points, 1, -1 do
                r[#r + 1] = points[i]
            end
            points = r
        end
        local r = rate(points)
        cands[#cands + 1] = r
        if not best or r.score > best.score then
            best = r
        end
        return r
    end
    -- Could a path this long still beat the best so far (if nothing muffled it)?
    local function couldBeat(len)
        return not best or 2 / math.max(len, 2) > best.score
    end
    if clear(L, S) and consider({ L, S }).shut == 0 then
        return best
    end
    local function done()
        return best, best and second(L, best, cands)
    end
    -- Over the top of a low wall or a parapet: barely bent, so it stays where it was.
    consider(overTop(L, S))
    consider(overTop(S, L), true)
    -- Doors and gaps in the walls in between.
    consider(route(L, S, ROUTE_DEPTH))
    -- Around a corner: one bend on our side, then on theirs.
    local function oneBend(list, sees)
        local sorted = sortedBy(list, function(p)
            return dist(L, p) + dist(p, S)
        end)
        for i = 1, math.min(10, #sorted) do
            local p = sorted[i]
            if not couldBeat(dist(L, p) + dist(p, S)) then
                break
            end
            if sees(p) and consider({ L, p, S }).shut == 0 then
                break
            end
        end
    end
    -- Probes are redone only every metre, so check they still see their end
    -- too: else what we hear would depend on where we came from (V3).
    oneBend(listenerProbes, function(p)
        return clear(p, S) and clear(L, p)
    end)
    oneBend(sp, function(q)
        return clear(L, q) and clear(q, S)
    end)
    if best and best.shut == 0 then
        return done()
    end
    -- Two corners: a point on our side that sees one on theirs.
    local nearS = sortedBy(listenerProbes, function(p)
        return dist(p, S)
    end)
    local nearL = sortedBy(sp, function(q)
        return dist(q, L)
    end)
    for i = 1, math.min(5, #nearS) do
        for j = 1, math.min(5, #nearL) do
            local p, q = nearS[i], nearL[j]
            if couldBeat(dist(L, p) + dist(p, q) + dist(q, S)) and clear(p, q) and clear(L, p) and clear(q, S) then
                consider({ L, p, q, S })
            end
        end
    end
    return done()
end

-- Thickness of solid between a and b along the straight line.
local function thickness(a, b)
    local h1, d1 = hit(a, b)
    local h2, d2 = hit(b, a)
    if not h1 or not h2 then
        return 0
    end
    return math.max(0, dist(a, b) - d1 - d2)
end

-- Thickness of the wall between L and S: the line may slip through a seam, so
-- the thickest of it and its shifted copies.
local function wallThickness(L, S)
    local t = thickness(L, S)
    for _, sh in ipairs(shifted(L, S)) do
        t = math.max(t, thickness(sh[1], sh[2]))
    end
    return t
end

-- A ray that starts inside a car stops at once and sees nothing beyond, so
-- we also cast from the middle (outside both cars when each of us sits in one)
-- out to each end.
local function carBetween(L, S)
    local a = { x = L.x, y = L.y, z = L.z - CAR_DROP }
    local b = { x = S.x, y = S.y, z = S.z - CAR_DROP }
    local m = { x = (a.x + b.x) / 2, y = (a.y + b.y) / 2, z = (a.z + b.z) / 2 }
    return hit(a, b, carGroups) ~= nil
        or hit(b, a, carGroups) ~= nil
        or hit(m, a, carGroups) ~= nil
        or hit(m, b, carGroups) ~= nil
end

-- A closed door across a -> b: something Dynamic inside the volume that
-- keeps the player off it. That volume can be thick and lopsided, and either
-- end may stand in it (a ray from inside doesn't see it): so we look at it
-- along the line stretched past both ends, and check the door lies in it.
local BLOCKER_MAX = 3.0
local function doorRay(a, b)
    local _, dd = hit(a, b, { doorGroups[2] })
    if not dd then
        return false
    end
    local len = dist(a, b)
    local dir = { x = (b.x - a.x) / len, y = (b.y - a.y) / len, z = (b.z - a.z) / len }
    local ea, eb = add(a, dir, -BLOCKER_MAX), add(b, dir, BLOCKER_MAX)
    local _, enter = hit(ea, eb, { doorGroups[1] })
    local _, back = hit(eb, ea, { doorGroups[1] })
    if not enter or not back then
        return false
    end
    local at, leave = dd + BLOCKER_MAX, len + 2 * BLOCKER_MAX - back
    return enter <= at + 0.3 and leave >= at - 0.3 and leave - enter < BLOCKER_MAX
end

-- A door seen by the line, or by its copies on both sides of a seam it slips through.
local function door(a, b)
    if doorRay(a, b) then
        return true
    end
    local s = shifted(a, b)
    return (doorRay(s[1][1], s[1][2]) and doorRay(s[2][1], s[2][2]))
        or (doorRay(s[3][1], s[3][2]) and doorRay(s[4][1], s[4][2]))
end

-- Thin panes and closed doors crossed along the path (thick things in the
-- glass groups are props, not windows, and the air goes around them).
function panes(path)
    local n, doors = 0, 0
    for i = 2, #path do
        local a, b = path[i - 1], path[i]
        local h1, d1 = hit(a, b, glassGroups)
        if h1 then
            local _, d2 = hit(b, a, glassGroups)
            if d2 and dist(a, b) - d1 - d2 <= GLASS_MAX_THICK then
                n = n + 1
            end
        end
        if door(a, b) then
            doors = doors + 1
        end
    end
    return n, doors
end

local function head(p, h)
    return { x = p.x, y = p.y, z = p.z + (h or HEAD) }
end

-- Crouched, when the camera can't be read (about where its eyes go).
local CROUCH = 1.0

-- Our ears above our feet: the first-person camera's height when it reads
-- sensibly (crouching, seated, ducking all count), else from crouching.
-- `A.earFrom` says which (camera, crouch, stand).
local function earHeight(player, feet)
    local ok, cam = pcall(function()
        return player:GetFPPCameraComponent():GetLocalToWorld().W
    end)
    if ok and cam and type(cam.z) == "number" then
        local h, off = cam.z - feet.z, math.abs(cam.x - feet.x) + math.abs(cam.y - feet.y)
        if h > 0.3 and h < 2.2 and off < 1.5 then
            return h, "camera"
        end
    end
    local okC, crouched = pcall(function()
        local defs = GetAllBlackboardDefs()
        local bb = Game.GetBlackboardSystem():GetLocalInstanced(player:GetEntityID(), defs.PlayerStateMachine)
        return bb:GetInt(defs.PlayerStateMachine.Locomotion) == EnumInt(gamePSMLocomotionStates.Crouch)
    end)
    if okC and type(crouched) == "boolean" then
        return crouched and CROUCH or HEAD, crouched and "crouch" or "stand"
    end
    return HEAD, "?"
end
A.ear, A.earFrom = HEAD, "?"

-- Where a path seems to come from (its first opening, as far as the sound
-- travelled), its loudness and cutoff.
local function describe(L, best)
    local path, len, turn = best.points, best.len, best.turn
    local first = path[2]
    local d = dist(L, first)
    local t = d > 1e-3 and len / d or 1
    local from = { x = L.x + (first.x - L.x) * t, y = L.y + (first.y - L.y) * t, z = L.z + (first.z - L.z) * t }
    -- Diffraction: quieter and darker the sharper the turn.
    local cutoff = 18000 * math.max(0.03, (1 - turn / 200) ^ 2)
    if best.glass > 0 then
        cutoff = math.min(cutoff, GLASS_CUTOFF)
    end
    if best.doors > 0 then
        cutoff = math.min(cutoff, DOOR_CUTOFF)
    end
    return from, best.gain, cutoff
end

-- `gain` followed in dB (see SMOOTH), per `key`.
local function smoothed(key, gain)
    local db = gain > 0 and math.max(SILENT_DB, 20 * math.log10(gain)) or SILENT_DB
    local prev = heardDb[key] or db
    prev = prev + (db - prev) * SMOOTH
    heardDb[key] = prev
    return prev > SILENT_DB + 0.5 and 10 ^ (prev / 20) or 0
end

-- Probes are redone as soon as anyone moves: probes from a metre back made what
-- we hear depend on where we came from (V3). Standing still reuses them.
local PROBE_MOVE = 0.05

local function updatePaths(L, speakers)
    local moved = not listenerProbes or dist(L, listenerProbes.origin) > PROBE_MOVE
    if moved then
        listenerProbes = probes(L)
        listenerProbes.origin = L
    end
    for _, s in ipairs(speakers) do
        local S = head(s.pos, s.ear)
        local cached = speakerProbes[s.id]
        if not cached or dist(S, cached.origin) > PROBE_MOVE then
            cached = probes(S)
            cached.origin = S
            speakerProbes[s.id] = cached
        end
        local best, other = airPath(L, S, cached)
        local from, gain, cutoff
        if best then
            from, gain, cutoff = describe(L, best)
        else
            local thick = wallThickness(L, S)
            from = S
            if thick <= THIN_WALL then
                gain, cutoff = 0.18, 500
            elseif thick <= MEDIUM_WALL then
                gain, cutoff = 0.06, 300
            else
                gain, cutoff = 0, 200
            end
        end
        local car = carBetween(L, S)
        if gain > 0 and car then
            gain, cutoff = gain * CAR_GAIN, math.min(cutoff, CAR_CUTOFF)
        end
        gain = smoothed(s.id, gain)
        api.setPath(s.id, from, gain, cutoff)
        -- The second way in, or silence there.
        local from2, gain2, cutoff2 = from, 0, cutoff
        if other then
            from2, gain2, cutoff2 = describe(L, other)
            if car then
                gain2, cutoff2 = gain2 * CAR_GAIN, math.min(cutoff2, CAR_CUTOFF)
            end
        end
        gain2 = smoothed(s.id .. "+", gain2)
        api.setPathAlt(s.id, from2, gain2, cutoff2)
    end
end

local DIRS = nil
-- Rays for the surroundings: walls (horizontal and 30° up) and sky
-- (60° up and straight up). Each entry: direction, and whether it looks at
-- the sky.
local function envDirs()
    if DIRS then
        return DIRS
    end
    DIRS = {}
    local function ring(count, elevation, sky, offset)
        local c, z = math.cos(math.rad(elevation)), math.sin(math.rad(elevation))
        for i = 0, count - 1 do
            local yaw = (i + (offset or 0)) * 2 * math.pi / count
            DIRS[#DIRS + 1] = { x = math.cos(yaw) * c, y = math.sin(yaw) * c, z = z, sky = sky }
        end
    end
    ring(8, 0, false)
    ring(8, 30, false, 0.5)
    ring(4, 60, true, 0.25)
    DIRS[#DIRS + 1] = { x = 0, y = 0, z = 1, sky = true, up = true }
    return DIRS
end

-- How reverberant the place is. Most real surroundings barely ring: sound
-- that escapes upwards never comes back, and soft or broken surfaces scatter
-- it. What rings is a space closed on all sides by hard surfaces: a tunnel,
-- an underpass, a car park, a concrete hall. So we want walls around (a car
-- park's may be 30 m away) and a near cover above, and anything weak counts as
-- nothing. The bigger the space, the longer and more audible the tail.
local NEAR_SURFACE = 2.0
local FAR_SURFACE = 25.0
local FAR_WALL = 40.0
local WET_FLOOR = 0.12
local WET_FULL = 0.45
-- Even a bare concrete hall rings far less than a reverb plugin's preset.
local WET_MAX = 0.45
-- A furnished room barely rings: at full clutter this much is left.
local CLUTTER_LEFT = 0.2
-- A narrow passage rings this much of a big hall's reverb; from this width
-- between the walls (metres, to the nearest wall on average) up, all of it.
local SMALL_LEFT = 0.4
local NARROW = 1.5
local WIDE = 8.0

local function smoothstep(a, b, x)
    local t = math.max(0, math.min(1, (x - a) / (b - a)))
    return t * t * (3 - 2 * t)
end

local function updateSurroundings(L)
    local walls, wallN, cover, coverN, total, n = 0, 0, 0, 0, 0, 0
    local skyOpen, skyWeight = 0, 0
    local width, widthN = 0, 0
    for _, d in ipairs(envDirs()) do
        local far = { x = L.x + d.x * ENV_REACH, y = L.y + d.y * ENV_REACH, z = L.z + d.z * ENV_REACH }
        local h, dd = hit(L, far)
        local w = d.up and 2 or 1
        local c = 0
        if h then
            local far = d.sky and FAR_SURFACE or FAR_WALL
            c = math.max(0, math.min(1, 1 - (dd - NEAR_SURFACE) / (far - NEAR_SURFACE)))
            total = total + w * dd
            if d.z == 0 then
                width, widthN = width + dd, widthN + 1
            end
        else
            -- Open directions make the space longer (a tunnel's two ends).
            total = total + w * ENV_REACH
        end
        n = n + w
        if d.sky then
            cover, coverN = cover + w * c, coverN + w
            skyWeight = skyWeight + w
            if not h then
                skyOpen = skyOpen + w
            end
        else
            walls, wallN = walls + c, wallN + 1
        end
    end
    local enclosure = walls / wallN
    local covered = cover / coverN
    -- Furniture, shelves, counters: low rays stopping well short of the walls
    -- that the head-height rays reach. Soft and broken surfaces soak the
    -- sound up and scatter it.
    local low = { x = L.x, y = L.y, z = L.z - LOW }
    local clutter, clutterN = 0, 0
    for _, d in ipairs(envDirs()) do
        if d.z == 0 then
            local far = { x = low.x + d.x * ENV_REACH, y = low.y + d.y * ENV_REACH, z = low.z }
            local _, dl = hit(low, far, clutterGroups)
            local _, dh = hit(L, { x = L.x + d.x * ENV_REACH, y = L.y + d.y * ENV_REACH, z = L.z })
            clutterN = clutterN + 1
            if dl and dl < (dh or ENV_REACH) - 1.0 then
                clutter = clutter + 1
            end
        end
    end
    clutter = clutter / clutterN
    local raw = enclosure ^ 2 * covered ^ 1.5
    local size = math.max(0, math.min(1, (total / n - 2) / 25))
    width = widthN > 0 and width / widthN or 0
    local big = SMALL_LEFT + (1 - SMALL_LEFT) * smoothstep(NARROW, WIDE, width)
    local wet = WET_MAX * smoothstep(WET_FLOOR, WET_FULL, raw) * big * (1 - (1 - CLUTTER_LEFT) * clutter)
    local damp = 0.55 - 0.25 * covered
    local k = 0.3
    env.wet = env.wet + (wet - env.wet) * k
    env.size = env.size + (size - env.size) * k
    env.damp = env.damp + (damp - env.damp) * k
    api.setSurroundings(env.wet, env.size, env.damp)
end

function A.report(err)
    local msg = tostring(err)
    if msg ~= A.error then
        A.error = msg
        api.event("warn", "Environment", "acoustics", msg)
    end
end

function A.update(dt, settings)
    timers.path = timers.path + dt
    timers.env = timers.env + dt
    local player = Game.GetPlayer()
    if not player then
        return
    end
    if not sqs then
        sqs = Game.GetSpatialQueriesSystem()
        groups = { CName.new("Static"), CName.new("Terrain") }
        carGroups = { CName.new("Vehicle") }
        glassGroups, clutterGroups = {}, { CName.new("Static"), CName.new("Dynamic"), CName.new("Terrain") }
        for _, g in ipairs(GLASS_NAMES) do
            glassGroups[#glassGroups + 1] = CName.new(g)
        end
        doorGroups = { CName.new(DOOR_GROUPS[1]), CName.new(DOOR_GROUPS[2]) }
        if not sqs then
            return
        end
    end
    local feet = player:GetWorldPosition()
    A.ear, A.earFrom = earHeight(player, feet)
    api.setEar(A.ear)
    local L = head(feet, A.ear)
    if settings.reverb ~= "off" and timers.env >= ENV_EVERY then
        timers.env = 0
        local ok, err = pcall(updateSurroundings, L)
        if not ok then
            A.report(err)
        end
    end
    if settings.occlusion ~= "off" and timers.path >= PATH_EVERY then
        timers.path = 0
        local speakers = api.speakers()
        -- Every voice gets its classic path, also those Steam Audio works
        -- out in the DLL: there it is the way around when Steam Audio finds
        -- none (its rebounds missing).
        local ok, err = pcall(updatePaths, L, speakers)
        if not ok then
            A.report(err)
        end
        -- Forget probes of people who stopped talking.
        local live = {}
        for _, s in ipairs(speakers) do
            live[s.id] = true
        end
        for id in pairs(speakerProbes) do
            if not live[id] then
                speakerProbes[id] = nil
                heardDb[id], heardDb[id .. "+"] = nil, nil
            end
        end
    end
end

function A.reset()
    listenerProbes = nil
    speakerProbes = {}
    heardDb = {}
    sqs = nil
end

return A
