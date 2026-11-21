-- First-run guided setup: microphone, output, how you talk. Shown when the
-- CET overlay opens until it is finished or skipped; can be run again from
-- the Generale tab.

local api = require("modules/api")
local tr = require("modules/i18n").tr

local W = { open = false, step = 1 }

local calib = nil -- { left = seconds, sum = dB, n = samples }
local capturing = false

local function STEPS()
    return {
        tr("Benvenuto", "Welcome"),
        tr("Microfono", "Microphone"),
        tr("Uscita", "Output"),
        tr("Come parli", "How you talk"),
        tr("Fatto", "Done"),
    }
end

function W.start()
    W.open = true
    W.finished = false
    W.step = 1
end

local function finish()
    api.set("setup_done", "on")
    api.set("monitor", "off")
    W.open = false
    -- The settings copy refreshes a moment later; don't reopen meanwhile.
    W.finished = true
    W.step = 1
end

-- Noise floor measurement for the open-mic threshold.
function W.update(dt, state)
    if not calib then
        return
    end
    calib.left = calib.left - dt
    calib.sum = calib.sum + state.meters.processed
    calib.n = calib.n + 1
    if calib.left <= 0 then
        local floor = calib.sum / math.max(1, calib.n)
        -- Comfortably above the silence, well below a normal voice.
        local threshold = math.max(-70, math.min(-20, floor + 12))
        api.set("vad_threshold_db", string.format("%.0f", threshold))
        W.calibrated = string.format(
            tr(
                "Rumore di fondo %.0f dB: soglia impostata a %.0f dB.",
                "Background noise %.0f dB: threshold set to %.0f dB."
            ),
            floor,
            threshold
        )
        calib = nil
    end
end

local function nav(w, state, canNext)
    ImGui.Spacing()
    ImGui.Separator()
    if W.step > 1 and ImGui.Button(tr("Indietro", "Back")) then
        W.step = W.step - 1
    end
    ImGui.SameLine()
    if W.step < #STEPS() then
        if ImGui.Button(tr("Avanti", "Next")) and canNext ~= false then
            W.step = W.step + 1
        end
        ImGui.SameLine()
        if ImGui.Button(tr("Salta la configurazione", "Skip setup")) then
            finish()
        end
    elseif ImGui.Button(tr("Fine", "Finish")) then
        finish()
    end
    if W.step == 1 then
        -- Language, bottom right.
        ImGui.SameLine()
        ImGui.SetCursorPosX(math.max(0, ImGui.GetWindowWidth() - 270))
        ImGui.Text("Language / Lingua")
        ImGui.SameLine()
        w.languageCombo(state.settings)
    end
end

function W.draw(state)
    local ui = require("modules/ui")
    local w = ui.w
    local s, m = state.settings, state.meters
    local pushed = w.theme()
    ImGui.SetNextWindowSize(560, 480, ImGuiCond.FirstUseEver)
    if ImGui.Begin(tr("VoiceChat - configurazione guidata", "VoiceChat - guided setup")) then
        ImGui.Text(string.format(tr("Passo %d di %d: %s", "Step %d of %d: %s"), W.step, #STEPS(), STEPS()[W.step]))
        ImGui.Separator()
        if W.step == 1 then
            ImGui.TextWrapped(
                tr(
                    "Ciao! VoiceChat è una chat vocale di prossimità per le sessioni co-op di NightCoop: senti gli altri giocatori vicino a te, dalla direzione in cui si trovano, attutiti dai muri.",
                    "Hi! VoiceChat is a proximity voice chat for NightCoop co-op sessions: you hear the players near you, from where they are, muffled by walls."
                )
            )
            ImGui.Spacing()
            ImGui.TextWrapped(
                tr(
                    "Non devi configurare server o indirizzi: la voce passa da NightCoop, cifrata, e si collega da sola quando sei in sessione. Nella lobby vi sentite tutti; nel mondo conta la distanza.",
                    "No servers or addresses to set up: voice goes through NightCoop, encrypted, and connects by itself when you are in a session. In the lobby everybody hears everybody; in the world distance matters."
                )
            )
            ImGui.Spacing()
            ImGui.TextWrapped(
                tr(
                    "In un minuto scegliamo microfono, cuffie e come vuoi parlare.",
                    "In a minute we pick your microphone, headphones and how you want to talk."
                )
            )
        elseif W.step == 2 then
            w.deviceCombo(tr("Microfono", "Microphone"), s, "input_device", true)
            w.meter(tr("Ingresso", "Input"), m.input)
            w.slider(tr("Guadagno (dB)", "Gain (dB)"), s, "input_gain_db", -20, 30, "%.1f dB")
            w.hint(
                tr(
                    "Parla normalmente: la barra dovrebbe arrivare circa a metà. Se resta bassa alza il guadagno, se arriva in fondo abbassalo.",
                    "Talk normally: the bar should reach about halfway. If it stays low raise the gain, if it hits the end lower it."
                )
            )
            w.checkbox(
                tr("Ascolta il mio microfono (prova, con le cuffie)", "Listen to my microphone (test, with headphones)"),
                s,
                "monitor"
            )
            w.hint(
                tr(
                    "Con l'ascolto attivo senti la tua voce come la sentiranno gli altri. Usalo solo con le cuffie, con le casse fischia.",
                    "While listening you hear your voice as the others will. Use it with headphones only: with speakers it howls."
                )
            )
        elseif W.step == 3 then
            w.deviceCombo(tr("Cuffie / casse", "Headphones / speakers"), s, "output_device", false)
            w.slider(tr("Volume della chat vocale", "Voice chat volume"), s, "voice_volume", 0, 2)
            w.hint(
                tr(
                    "È il volume degli altri giocatori; ognuno si può regolare anche a parte, nella scheda Giocatori.",
                    "This is the volume of the other players; each one can also be set on their own, in the Players tab."
                )
            )
        elseif W.step == 4 then
            local open = s.mode == "open"
            if
                ImGui.Button(
                    open and tr("Passa al push-to-talk", "Switch to push-to-talk")
                        or tr("Passa al microfono aperto", "Switch to open mic")
                )
            then
                api.set("mode", open and "ptt" or "open")
            end
            if not open then
                ImGui.Text(
                    tr("Push-to-talk: parli tenendo premuto un tasto.", "Push-to-talk: you talk while holding a key.")
                )
                local label = capturing
                        and tr(
                            "Premi un tasto o un pulsante del mouse... (Esc annulla)",
                            "Press a key or a mouse button... (Esc cancels)"
                        )
                    or (
                        tr("Tasto: ", "Key: ")
                        .. (s.ptt or "?")
                        .. tr("  (clicca per cambiarlo)", "  (click to change it)")
                    )
                if ImGui.Button(label) and not capturing then
                    capturing = true
                    api.captureKey()
                end
                if capturing then
                    local vk = api.capturedKey()
                    if vk ~= 0 then
                        capturing = false
                        if vk > 0 then
                            api.set("ptt_vk", tostring(vk))
                        end
                    end
                end
                w.hint(
                    tr(
                        "Prova a tenerlo premuto: la barra qui sotto diventa azzurra.",
                        "Try holding it: the bar below turns cyan."
                    )
                )
            else
                ImGui.Text(
                    tr(
                        "Microfono aperto: trasmetti quando parli, da solo.",
                        "Open mic: you transmit when you talk, by itself."
                    )
                )
                if calib then
                    ImGui.Text(
                        string.format(tr("Resta in silenzio... %.0f", "Stay silent... %.0f"), math.max(0, calib.left))
                    )
                elseif
                    ImGui.Button(
                        tr(
                            "Calibra la soglia (3 secondi di silenzio)",
                            "Calibrate the threshold (3 seconds of silence)"
                        )
                    )
                then
                    calib = { left = 3, sum = 0, n = 0 }
                    W.calibrated = nil
                end
                if W.calibrated then
                    ImGui.Text(W.calibrated)
                end
                w.slider(tr("Soglia (dB)", "Threshold (dB)"), s, "vad_threshold_db", -80, 0, "%.0f dB")
                w.hint(
                    tr(
                        "Poi parla: la barra diventa azzurra quando superi la soglia.",
                        "Then talk: the bar turns cyan when you go above the threshold."
                    )
                )
            end
            w.meter(tr("Dopo l'elaborazione", "After processing"), m.processed, m.threshold)
            if m.gate then
                ImGui.TextColored(0.3686, 0.9647, 1.0, 1, tr("Ti sento!", "I can hear you!"))
            end
        else
            ImGui.TextWrapped(
                tr(
                    "Fatto! Quando ospiti o entri in una sessione co-op di NightCoop, la chat vocale si collega da sola.",
                    "Done! When you host or join a NightCoop co-op session, the voice chat connects by itself."
                )
            )
            ImGui.Spacing()
            ImGui.TextWrapped(
                tr(
                    "Nell'HUD, l'icona del microfono si illumina quando trasmetti. Tutte le opzioni (party, volumi per giocatore, eco, rumore, muri, HUD) sono nell'overlay di CET, finestra VoiceChat. Se qualcosa non va, la scheda Diagnostica dice cosa fare.",
                    "In the HUD, the microphone icon lights up when you transmit. All options (party, per-player volume, echo, noise, walls, HUD) are in the CET overlay, VoiceChat window. If something is wrong, the Diagnostics tab says what to do."
                )
            )
        end
        nav(w, state)
    end
    ImGui.End()
    ImGui.PopStyleColor(pushed)
end

return W
