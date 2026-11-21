-- Interface language: English (default) or Italian, from the `language`
-- setting. Strings are written in place as tr("italiano", "english").

local I = { lang = "en" }

function I.tr(it, en)
    if I.lang == "it" then
        return it
    end
    return en
end

return I
