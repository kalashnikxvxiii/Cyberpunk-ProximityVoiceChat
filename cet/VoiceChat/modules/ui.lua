-- Settings window in the CET overlay, one tab per topic.

local api = require("modules/api")
local tr = require("modules/i18n").tr
local acoustics = require("modules/acoustics")

local U = {}

local capturing = false
local partyInput = ""
local inputs = {}
local lastError = nil

local function NOISE()
    return {
        { "off", tr("Disattivata", "Off") },
        { "low", tr("Leggera", "Light") },
        { "medium", tr("Media", "Medium") },
        { "high", tr("Forte", "Strong") },
        { "max", tr("Massima", "Maximum") },
        { "ai", tr("IA (RNNoise)", "AI (RNNoise)") },
    }
end

local function set(key, value)
    lastError = api.set(key, value)
end

local function on(v)
    return v == "on"
end

local function checkbox(label, s, key)
    local value, pressed = ImGui.Checkbox(label, on(s[key]))
    if pressed then
        set(key, value and "on" or "off")
        s[key] = value and "on" or "off"
    end
end

local function slider(label, s, key, lo, hi, fmt)
    local cur = tonumber(s[key]) or lo
    local value, used = ImGui.SliderFloat(label, cur, lo, hi, fmt or "%.2f")
    if used and value ~= cur then
        set(key, string.format("%.3f", value))
        s[key] = tostring(value)
    end
end

local function hint(textLine)
    ImGui.PushStyleColor(ImGuiCol.Text, 0.6, 0.62, 0.66, 1)
    ImGui.TextWrapped(textLine)
    ImGui.PopStyleColor(1)
end

local function heading(textLine)
    ImGui.Spacing()
    ImGui.PushStyleColor(ImGuiCol.Text, 0.3686, 0.9647, 1.0, 1)
    ImGui.Text(textLine)
    ImGui.PopStyleColor(1)
    ImGui.Separator()
end

-- Level meter: -70 dB .. 0 dB.
local function meter(label, db, threshold)
    local frac = math.max(0, math.min(1, (db + 70) / 70))
    local overlay = string.format("%s  %.0f dB", label, db)
    if threshold and db > threshold then
        ImGui.PushStyleColor(ImGuiCol.PlotHistogram, 0.3686, 0.9647, 1.0, 1)
    else
        ImGui.PushStyleColor(ImGuiCol.PlotHistogram, 1.0, 0.38, 0.35, 1)
    end
    ImGui.ProgressBar(frac, -1, 0, overlay)
    ImGui.PopStyleColor(1)
end

-- CET's Selectable(label, selected) returns the item's selected state, so
-- the current item answers true every frame: act only on a different one.
local function picked(label, selected)
    return ImGui.Selectable(label, selected) and not selected
end

local function deviceCombo(label, s, key, input)
    local current = s[key] or ""
    local preview = current == "" and tr("Predefinito di Windows", "Windows default") or current
    if ImGui.BeginCombo(label, preview) then
        if picked(tr("Predefinito di Windows", "Windows default"), current == "") then
            set(key, "")
            s[key] = ""
        end
        for _, name in ipairs(api.devices(input)) do
            if picked(name, name == current) then
                set(key, name)
                s[key] = name
            end
        end
        ImGui.EndCombo()
    end
end

local function playerRow(p, s)
    ImGui.PushID("player" .. tostring(p.id))
    if p.talking then
        ImGui.PushStyleColor(ImGuiCol.Text, 0.3686, 0.9647, 1.0, 1)
    else
        ImGui.PushStyleColor(ImGuiCol.Text, 0.35, 0.35, 0.38, 1)
    end
    ImGui.Bullet()
    ImGui.PopStyleColor(1)
    ImGui.SameLine()
    local label = p.name .. (p.me and "  (tu)" or "")
    if p.party ~= "" then
        label = label .. "  [" .. p.party .. "]"
    end
    ImGui.Text(label)
    if not p.me then
        ImGui.SameLine(260)
        ImGui.PushItemWidth(180)
        local vol, used = ImGui.SliderFloat("##vol", p.volume, 0, 3, tr("volume %.2f", "volume %.2f"))
        ImGui.PopItemWidth()
        if used then
            set("volume." .. p.name, string.format("%.3f", vol))
        end
        ImGui.SameLine()
        local muted, pressed = ImGui.Checkbox(tr("Silenzia", "Mute"), p.muted)
        if pressed then
            set("mute." .. p.name, muted and "on" or "off")
        end
    end
    ImGui.PopID()
end

local function textInput(label, s, key, width)
    local cur = inputs[key] or s[key] or ""
    if width then
        ImGui.PushItemWidth(width)
    end
    local value, changed = ImGui.InputText(label, cur, 200)
    if width then
        ImGui.PopItemWidth()
    end
    local deactivated = ImGui.IsItemDeactivatedAfterEdit
    if changed then
        inputs[key] = value
        if not deactivated then
            set(key, value)
            s[key] = value
        end
    end
    -- Saved when editing stops, so a half-typed address is never used.
    if deactivated and deactivated() and inputs[key] then
        set(key, inputs[key])
        s[key] = inputs[key]
        inputs[key] = nil
    end
end

local function MODE_TEXT()
    return {
        host = tr("Sei l'host della sessione", "You are the host of the session"),
        guest = tr("Sei un ospite della sessione", "You are a guest of the session"),
        manual = tr("Modalità manuale", "Manual mode"),
        offline = tr("Non collegato", "Not connected"),
    }
end

local function LINK_TEXT()
    return {
        joined = tr("Collegato", "Connected"),
        connecting = tr("Collegamento in corso...", "Connecting..."),
        failed = tr("Errore", "Error"),
        off = tr("Spento", "Off"),
    }
end

local languageCombo -- defined below, used by tabConnection

local function tabConnection(state)
    local s, st = state.settings, state.status
    heading(tr("Stato", "Status"))
    ImGui.Text(tr("NightCoop: ", "NightCoop: ") .. (st.nightcoop or "?"))
    ImGui.Text(
        (MODE_TEXT()[st.mode or ""] or (st.mode or ""))
            .. ((st.mode_detail or "") ~= "" and (" - " .. st.mode_detail) or "")
    )
    local link = LINK_TEXT()[st.link or ""] or (st.link or "?")
    if st.link == "joined" then
        ImGui.TextColored(0.3686, 0.9647, 1.0, 1, link .. ((st.via or "") ~= "" and (": " .. st.via) or ""))
    elseif st.link == "failed" then
        ImGui.TextColored(1.0, 0.38, 0.35, 1, link .. ": " .. (st.link_detail or ""))
    else
        ImGui.Text(link .. ((st.link_detail or "") ~= "" and (" (" .. st.link_detail .. ")") or ""))
    end
    ImGui.Text(tr("Audio: ", "Audio: ") .. (st.audio == "ok" and "ok" or (st.audio or "?")))
    hint(
        tr(
            "Se qualcosa non va, la scheda Diagnostica dice cosa e come risolverlo.",
            "If something is wrong, the Diagnostics tab says what and how to fix it."
        )
    )

    heading(tr("Trasmissione", "Transmission"))
    local modes = { { "ptt", tr("Push-to-talk", "Push-to-talk") }, { "open", tr("Microfono aperto", "Open mic") } }
    local preview = s.mode == "open" and tr("Microfono aperto", "Open mic") or tr("Push-to-talk", "Push-to-talk")
    if ImGui.BeginCombo(tr("Modalità", "Mode"), preview) then
        for _, m in ipairs(modes) do
            if picked(m[2], s.mode == m[1]) then
                set("mode", m[1])
                s.mode = m[1]
            end
        end
        ImGui.EndCombo()
    end
    if s.mode ~= "open" then
        local label = capturing
                and tr(
                    "Premi un tasto o un pulsante del mouse... (Esc annulla)",
                    "Press a key or a mouse button... (Esc cancels)"
                )
            or (tr("Tasto: ", "Key: ") .. (s.ptt or "?"))
        if ImGui.Button(label) and not capturing then
            capturing = true
            api.captureKey()
        end
        if capturing then
            local vk = api.capturedKey()
            if vk ~= 0 then
                capturing = false
                if vk > 0 then
                    set("ptt_vk", tostring(vk))
                end
            end
        end
        local clash = state.status and state.status.ptt_clash or ""
        if clash ~= "" then
            ImGui.TextColored(1, 0.75, 0.3, 1, tr("Lo usa anche NightCoop: ", "NightCoop uses it too: ") .. clash)
            hint(tr("Premendolo farai anche quello: scegline un altro.", "Pressing it does that too: pick another."))
        end
    else
        hint(
            tr(
                "Il microfono si attiva da solo quando superi la soglia (scheda Audio).",
                "The mic opens by itself when you go above the threshold (Audio tab)."
            )
        )
    end
    checkbox(tr("Microfono disattivato", "Microphone muted"), s, "mic_muted")
    checkbox(tr("Silenzia la chat vocale", "Mute voice chat"), s, "deafened")
    if on(s.deafened) then
        hint(
            tr(
                "Con la chat silenziata anche il tuo microfono è spento.",
                "With the chat muted your microphone is off too."
            )
        )
    end

    heading(tr("Sessione", "Session"))
    local auto = s.session_mode ~= "manual"
    local value, pressed = ImGui.Checkbox(
        tr("Segui la sessione NightCoop (consigliato)", "Follow the NightCoop session (recommended)"),
        auto
    )
    if pressed then
        set("session_mode", value and "auto" or "manual")
        s.session_mode = value and "auto" or "manual"
    end
    if auto then
        hint(
            tr(
                "La voce passa da NightCoop, cifrata: nessun indirizzo da scambiare, nessuna porta da aprire. Non serve configurare nulla.",
                "Voice goes through NightCoop, encrypted: no addresses to exchange, no ports to open. Nothing to configure."
            )
        )
    else
        textInput(tr("Server (indirizzo:porta#impronta)", "Server (address:port#fingerprint)"), s, "server")
        textInput(tr("Codice stanza", "Room code"), s, "room")
        textInput(tr("Nome (vuoto = nome NightCoop)", "Name (empty = NightCoop name)"), s, "name")
        if (s.server_key or "") ~= "" then
            ImGui.Text(tr("Chiave del server salvata: ", "Saved server key: ") .. s.server_key)
            if ImGui.Button(tr("Dimentica la chiave del server", "Forget the server key")) then
                set("server_key", "")
            end
        end
        hint(
            tr(
                "Chi ha lo stesso server e lo stesso codice stanza si sente. Server, stanza e nome valgono subito.",
                "People with the same server and room code hear each other. Server, room and name apply right away."
            )
        )
    end
    heading(tr("Lingua", "Language"))
    languageCombo(s)
    ImGui.Spacing()
    if ImGui.Button(tr("Rifai la configurazione guidata", "Run the guided setup again")) then
        require("modules/wizard").start()
    end
end

local LEVEL_COLOR = {
    error = { 1.0, 0.38, 0.35 },
    warn = { 1.0, 0.78, 0.30 },
    info = { 0.3686, 0.9647, 1.0 },
}
-- Phases come in English (they are in the log too); Italian for the table.
local function PHASE(p)
    local it = { Session = "Sessione", Environment = "Ambiente" }
    return tr(it[p] or p, p)
end

-- Language picker, shared with the guided setup.
function languageCombo(s)
    local names = { en = "English", it = "Italiano" }
    local cur = s.language == "it" and "it" or "en"
    ImGui.PushItemWidth(140)
    if ImGui.BeginCombo("##language", names[cur]) then
        for _, code in ipairs({ "en", "it" }) do
            if picked(names[code], code == cur) then
                set("language", code)
                s.language = code
                require("modules/i18n").lang = code
            end
        end
        ImGui.EndCombo()
    end
    ImGui.PopItemWidth()
end

local function LEVEL_TEXT()
    return { error = tr("ERRORE", "ERROR"), warn = tr("AVVISO", "WARNING"), info = "info" }
end

local function tabDiagnostics(state)
    local st = state.status
    if (st.nc_build or "") ~= "" then
        heading("NightCoop")
        ImGui.Text(tr("Build: ", "Build: ") .. st.nc_build .. "  (SDK v1)")
        ImGui.TextWrapped(tr("Stato: ", "State: ") .. (st.nightcoop or "?"))
        if (st.nc_members or "") ~= "" then
            ImGui.TextWrapped(tr("Giocatori: ", "Players: ") .. st.nc_members)
        end
        if (st.nc_voice or "") ~= "" then
            ImGui.TextWrapped(tr("Voce: ", "Voice: ") .. st.nc_voice)
        end
    end
    heading(tr("Registro", "Log"))
    local list = api.events()
    if ImGui.Button(tr("Svuota", "Clear")) then
        api.clearEvents()
    end
    ImGui.SameLine()
    hint(
        tr(
            "Più recenti in alto. I problemi risolti spariscono da soli.",
            "Newest first. Solved problems disappear by themselves."
        )
    )
    if #list == 0 then
        hint(tr("Nessun problema.", "No problems."))
        return
    end
    local useTable = ImGui.BeginTable
        and ImGui.BeginTable(
            "vc_events",
            4,
            (ImGuiTableFlags and (ImGuiTableFlags.Borders + ImGuiTableFlags.RowBg + ImGuiTableFlags.Resizable)) or 0
        )
    if useTable then
        ImGui.TableSetupColumn("Ora")
        ImGui.TableSetupColumn(tr("Fase", "Phase"))
        ImGui.TableSetupColumn(tr("Cosa è successo", "What happened"))
        ImGui.TableSetupColumn(tr("Cosa puoi fare", "What you can do"))
        ImGui.TableHeadersRow()
    end
    for _, e in ipairs(list) do
        local c = LEVEL_COLOR[e.level] or LEVEL_COLOR.info
        local what = (LEVEL_TEXT()[e.level] or e.level)
            .. ": "
            .. e.detail
            .. (e.count > 1 and string.format(" (x%d)", e.count) or "")
        if useTable then
            ImGui.TableNextRow()
            ImGui.TableNextColumn()
            ImGui.Text(e.time)
            ImGui.TableNextColumn()
            ImGui.Text(PHASE(e.phase))
            ImGui.TableNextColumn()
            ImGui.PushStyleColor(ImGuiCol.Text, c[1], c[2], c[3], 1)
            ImGui.TextWrapped(what)
            ImGui.PopStyleColor(1)
            ImGui.TableNextColumn()
            ImGui.TextWrapped(e.hint ~= "" and e.hint or "-")
        else
            ImGui.PushStyleColor(ImGuiCol.Text, c[1], c[2], c[3], 1)
            ImGui.TextWrapped(e.time .. "  [" .. PHASE(e.phase) .. "]  " .. what)
            ImGui.PopStyleColor(1)
            if e.hint ~= "" then
                hint("   -> " .. e.hint)
            end
            ImGui.Separator()
        end
    end
    if useTable then
        ImGui.EndTable()
    end
end

local function tabAudio(state)
    local s, m = state.settings, state.meters
    heading(tr("Dispositivi", "Devices"))
    deviceCombo(tr("Ingresso", "Input"), s, "input_device", true)
    deviceCombo(tr("Uscita", "Output"), s, "output_device", false)
    if ImGui.Button(tr("Aggiorna elenco", "Refresh list")) then
        api.rescan()
    end

    heading(tr("Microfono", "Microphone"))
    meter(tr("Ingresso", "Input"), m.input)
    meter(tr("Dopo l'elaborazione", "After processing"), m.processed, m.threshold)
    slider(tr("Guadagno (dB)", "Gain (dB)"), s, "input_gain_db", -20, 30, "%.1f dB")
    slider(tr("Volume del microfono", "Microphone volume"), s, "mic_volume", 0, 2)
    slider(tr("Sensibilità: soglia (dB)", "Sensitivity: threshold (dB)"), s, "vad_threshold_db", -80, 0, "%.0f dB")
    hint(
        tr(
            "La soglia vale per il microfono aperto: la barra diventa azzurra quando la superi.",
            "The threshold is for open mic: the bar turns cyan when you go above it."
        )
    )
    checkbox(tr("Ascolta il mio microfono (prova)", "Listen to my microphone (test)"), s, "monitor")
    hint(
        tr(
            "Solo con le cuffie: con le casse il microfono riprende se stesso. Se il segnale va in saturazione per quasi un secondo, l'ascolto si spegne da solo.",
            "Headphones only: with speakers the mic picks itself up. If the signal saturates for almost a second, listening turns itself off."
        )
    )
    if m.transmitting then
        ImGui.TextColored(0.3686, 0.9647, 1.0, 1, tr("In onda", "On air"))
    elseif m.gate then
        ImGui.TextColored(1.0, 0.38, 0.35, 1, tr("Attivo (non collegato o fuori dal gioco)", "Active (not connected)"))
    else
        ImGui.Text(tr("In silenzio", "Silent"))
    end

    heading(tr("Chat vocale", "Voice chat"))
    slider(tr("Volume della chat vocale", "Voice chat volume"), s, "voice_volume", 0, 2)
end

local function tabProcessing(state)
    local s, m = state.settings, state.meters
    heading(tr("Elaborazione del microfono", "Microphone processing"))
    checkbox(tr("Cancellazione dell'eco", "Echo cancellation"), s, "echo_cancel")
    hint(
        tr(
            "Toglie dal microfono le voci degli altri che escono dalle casse. Con le cuffie si può spegnere.",
            "Removes the others' voices coming out of your speakers from your mic. With headphones you can turn it off."
        )
    )
    local current = s.noise or "medium"
    local preview = current
    for _, n in ipairs(NOISE()) do
        if n[1] == current then
            preview = n[2]
        end
    end
    if ImGui.BeginCombo(tr("Soppressione del rumore", "Noise suppression"), preview) then
        for _, n in ipairs(NOISE()) do
            if picked(n[2], n[1] == current) then
                set("noise", n[1])
                s.noise = n[1]
            end
        end
        ImGui.EndCombo()
    end
    hint(
        tr(
            "Leggera...Massima: filtro classico (ventole, fruscio). IA: rete neurale, toglie anche tastiera e rumori improvvisi.",
            "Light...Maximum: classic filter (fans, hiss). AI: neural network, also removes keyboard and sudden noises."
        )
    )
    meter(tr("Ingresso", "Input"), m.input)
    meter(tr("Dopo l'elaborazione", "After processing"), m.processed, m.threshold)
end

local function tabParty(state)
    local s = state.settings
    heading(tr("Il tuo party", "Your party"))
    if (s.party or "") ~= "" then
        ImGui.Text(tr("Party: ", "Party: ") .. s.party)
        if ImGui.Button(tr("Esci dal party", "Leave the party")) then
            set("party", "")
        end
    else
        ImGui.Text(tr("Non sei in un party.", "You are not in a party."))
    end
    partyInput = ImGui.InputText(tr("Nome del party", "Party name"), partyInput, 32)
    if ImGui.Button(tr("Crea / entra", "Create / join")) and partyInput ~= "" then
        set("party", partyInput)
    end
    hint(
        tr(
            "Chi scrive lo stesso nome entra nello stesso party: vi sentite a qualsiasi distanza, come una radio quando siete lontani.",
            "Whoever types the same name joins the same party: you hear each other at any distance, like a radio when far apart."
        )
    )

    heading(tr("Membri", "Members"))
    local any = false
    for _, p in ipairs(state.players) do
        if (s.party or "") ~= "" and p.party == s.party then
            playerRow(p, s)
            any = true
        end
    end
    if not any then
        hint(tr("Nessuno nel party.", "Nobody in the party."))
    end
end

local function tabPlayers(state)
    heading(tr("Giocatori nella stanza", "Players in the room"))
    if #state.players == 0 then
        hint(tr("Nessuno (non collegato?).", "Nobody (not connected?)."))
    end
    for _, p in ipairs(state.players) do
        playerRow(p, state.settings)
    end
end

local function tabEnvironment(state)
    local s = state.settings
    heading(tr("Propagazione della voce", "Voice propagation"))
    checkbox(tr("Muri e aperture", "Walls and openings"), s, "occlusion")
    hint(
        tr(
            "La voce passa da porte e aperture vicine, attutita; un muro sottile la smorza, uno spesso la blocca.",
            "Voice goes through nearby doors and openings, muffled; a thin wall dampens it, a thick one blocks it."
        )
    )
    checkbox(tr("Acustica realistica (Steam Audio)", "Realistic acoustics (Steam Audio)"), s, "steam_audio")
    hint(
        tr(
            "La voce rimbalza su muri e soffitti, esce da porte e vetrine come nella realtà, e i materiali contano (vetro, metallo, cemento). Spento: il modello di prima, più leggero.",
            "Voices bounce off walls and ceilings, come out of doors and shop windows as in real life, and materials count (glass, metal, concrete). Off: the previous, lighter model."
        )
    )
    slider(tr("Quanto rimbombano i luoghi", "How much places ring"), s, "rebounds", 0, 2)
    checkbox(tr("Riverbero degli ambienti", "Room reverb"), s, "reverb")
    checkbox(tr("Senti il riverbero della tua voce", "Hear the reverb of your own voice"), s, "self_reverb")
    slider(tr("Quanto", "How much"), s, "self_reverb_level", 0, 1)

    if acoustics.error then
        ImGui.TextColored(1, 0.4, 0.4, 1, tr("Errore: ", "Error: ") .. acoustics.error)
    end
end

local function tabHud(state, hud)
    local s = state.settings
    heading(tr("Icona nell'HUD", "HUD icon"))
    checkbox(tr("Mostra", "Show"), s, "hud")
    slider(tr("Posizione X", "Position X"), s, "hud_x", 0, 1920, "%.0f")
    slider(tr("Posizione Y", "Position Y"), s, "hud_y", 0, 1080, "%.0f")
    slider(tr("Scala", "Scale"), s, "hud_scale", 0.5, 2.5)
    slider(tr("Opacità", "Opacity"), s, "hud_opacity", 0.1, 1)
    hint(
        tr(
            "Le coordinate sono su 1920x1080 a prescindere dalla risoluzione.",
            "Coordinates are on 1920x1080 whatever the resolution."
        )
    )
    ImGui.Text(
        tr("Agganciata all'HUD: ", "Attached to the HUD: ")
            .. (
                hud.attached and tr("sì", "yes")
                or (
                    hud.hooked and tr("no (errore)", "no (error)")
                    or tr("non ancora (carica un salvataggio)", "not yet (load a save)")
                )
            )
    )
    if
        ImGui.Button(
            tr(
                "Trova l'icona (al centro, ingrandita, per 4 secondi)",
                "Find the icon (centred, enlarged, for 4 seconds)"
            )
        )
    then
        hud.flash()
    end
    if hud.error then
        ImGui.TextColored(1, 0.4, 0.4, 1, tr("Errore HUD: ", "HUD error: ") .. hud.error)
    end
end

local function theme()
    local c = ImGuiCol
    local colors = {
        { c.WindowBg, 0.03, 0.04, 0.06, 0.95 },
        { c.TitleBg, 0.10, 0.03, 0.04, 1 },
        { c.TitleBgActive, 0.30, 0.07, 0.08, 1 },
        { c.Border, 1.0, 0.38, 0.35, 0.45 },
        { c.FrameBg, 0.12, 0.05, 0.06, 1 },
        { c.FrameBgHovered, 0.25, 0.08, 0.09, 1 },
        { c.FrameBgActive, 0.35, 0.10, 0.11, 1 },
        { c.Button, 0.35, 0.09, 0.10, 1 },
        { c.ButtonHovered, 0.55, 0.14, 0.15, 1 },
        { c.ButtonActive, 0.75, 0.20, 0.20, 1 },
        { c.Header, 0.35, 0.09, 0.10, 1 },
        { c.HeaderHovered, 0.55, 0.14, 0.15, 1 },
        { c.Tab, 0.18, 0.05, 0.06, 1 },
        { c.TabHovered, 0.55, 0.14, 0.15, 1 },
        { c.TabActive, 0.75, 0.20, 0.20, 1 },
        { c.SliderGrab, 0.3686, 0.9647, 1.0, 1 },
        { c.SliderGrabActive, 0.6, 1.0, 1.0, 1 },
        { c.CheckMark, 0.3686, 0.9647, 1.0, 1 },
        { c.Separator, 1.0, 0.38, 0.35, 0.35 },
        { c.Text, 0.94, 0.93, 0.90, 1 },
    }
    local pushed = 0
    for _, col in ipairs(colors) do
        -- Some names differ between ImGui versions; skip what this one lacks.
        if col[1] ~= nil then
            ImGui.PushStyleColor(col[1], col[2], col[3], col[4], col[5])
            pushed = pushed + 1
        end
    end
    return pushed
end

function U.draw(state, hud)
    local pushed = theme()
    ImGui.SetNextWindowSize(560, 620, ImGuiCond.FirstUseEver)
    if ImGui.Begin("VoiceChat " .. (api.version or "")) then
        if not api.available then
            ImGui.TextWrapped(
                tr(
                    "VoiceChat.dll non risponde: controlla che sia in red4ext\\plugins\\VoiceChat.",
                    "VoiceChat.dll is not answering: check that it is in red4ext\\plugins\\VoiceChat."
                )
            )
            if api.error then
                hint(api.error)
            end
        elseif ImGui.BeginTabBar("vc_tabs") then
            local tabs = {
                { tr("Generale", "General"), tabConnection },
                { tr("Audio", "Audio"), tabAudio },
                { tr("Elaborazione", "Processing"), tabProcessing },
                { tr("Party", "Party"), tabParty },
                { tr("Giocatori", "Players"), tabPlayers },
                { tr("Ambiente", "Environment"), tabEnvironment },
                { "HUD", tabHud },
                { tr("Diagnostica", "Diagnostics"), tabDiagnostics },
            }
            for _, t in ipairs(tabs) do
                if ImGui.BeginTabItem(t[1]) then
                    t[2](state, hud)
                    ImGui.EndTabItem()
                end
            end
            ImGui.EndTabBar()
        end
        if lastError then
            ImGui.TextColored(1, 0.4, 0.4, 1, lastError)
        end
    end
    ImGui.End()
    ImGui.PopStyleColor(pushed)
end

-- Shared with the guided setup.
U.w = {
    set = set,
    on = on,
    checkbox = checkbox,
    slider = slider,
    hint = hint,
    heading = heading,
    meter = meter,
    picked = picked,
    deviceCombo = deviceCombo,
    theme = theme,
    languageCombo = languageCombo,
}

return U
