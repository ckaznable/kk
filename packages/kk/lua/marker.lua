local utils = require 'mp.utils'

-- ==========================================
-- 1. Config
-- ==========================================
local HIDE_TIMEOUT = 2.0
local BAR_HEIGHT = 20
local BAR_MARGIN_X = 5   -- Horizontal margin
local BAR_MARGIN_Y = 25   -- Height from bottom
local HITBOX_SIZE = 10    -- Hitbox tolerance size

local markers = {}
local user_active = true
local hide_timer = nil

-- Create Overlay object
local ov = mp.create_osd_overlay("ass-events")

-- Force disable built-in OSC
mp.set_property("osc", "no")

-- ==========================================
-- 2. Auto-hide logic
-- ==========================================
function on_timeout()
    user_active = false
    ov.data = ""
    ov:update()
    -- Notify Rust side that UI is hidden
    mp.commandv("script-message", "ui_visibility_changed", "hidden")
end

function reset_activity(prop_name, val)
    local was_inactive = not user_active
    user_active = true
    if hide_timer then hide_timer:kill() end
    hide_timer = mp.add_timeout(HIDE_TIMEOUT, on_timeout)
    
    if was_inactive then
        -- Notify Rust side that UI is visible
        mp.commandv("script-message", "ui_visibility_changed", "visible")

        -- If woke up by mouse movement, send specific event
        if prop_name == "mouse-pos" then
            mp.commandv("script-message", "mouse_move_wake")
        end
    end
    
    draw_ui()
end

-- ==========================================
-- 3. Drawing Logic (Overlay Rendering)
-- ==========================================
function draw_ui()
    if not user_active then return end

    local dur = mp.get_property_number("duration") or 0
    local pos = mp.get_property_number("time-pos") or 0
    if dur <= 0 then 
        ov.data = ""
        ov:update()
        return 
    end

    local w, h = mp.get_osd_size()
    ov.res_x = w
    ov.res_y = h

    -- Coordinate calculation (must match click logic)
    local bar_w = w - (BAR_MARGIN_X * 2)
    local bar_x = BAR_MARGIN_X
    local bar_y = h - BAR_MARGIN_Y

    local ass_lines = {}
    local common_style = "{\\an7\\pos(0,0)\\bord0\\shad0}"

    -- Layer 1: Background (Dark Grey)
    local bg_ass = common_style .. "{\\1c&H444444&}"
    bg_ass = bg_ass .. string.format("{\\p1}m %d %d l %d %d l %d %d l %d %d{\\p0}",
        bar_x, bar_y, 
        bar_x + bar_w, bar_y, 
        bar_x + bar_w, bar_y + BAR_HEIGHT, 
        bar_x, bar_y + BAR_HEIGHT
    )
    table.insert(ass_lines, bg_ass)

    -- Layer 2: Progress (Blue/Yellow)
    local prog_pct = pos / dur
    if prog_pct > 1 then prog_pct = 1 end
    if prog_pct < 0 then prog_pct = 0 end
    
    if prog_pct > 0 then
        local prog_w = bar_w * prog_pct
        local fg_ass = common_style .. "{\\1c&H00AADD&}"
        fg_ass = fg_ass .. string.format("{\\p1}m %d %d l %d %d l %d %d l %d %d{\\p0}",
            bar_x, bar_y, 
            bar_x + prog_w, bar_y, 
            bar_x + prog_w, bar_y + BAR_HEIGHT, 
            bar_x, bar_y + BAR_HEIGHT
        )
        table.insert(ass_lines, fg_ass)
    end

    -- Layer 3: Markers (White)
    for _, time in ipairs(markers) do
        local pct = time / dur
        if pct >= 0 and pct <= 1 then
            local mx = bar_x + (pct * bar_w)
            local my_top = bar_y - 4
            local my_bot = bar_y + BAR_HEIGHT + 4
            
            local mark_ass = common_style .. "{\\1c&HFFFFFF&}"
            mark_ass = mark_ass .. string.format("{\\p1}m %d %d l %d %d l %d %d l %d %d{\\p0}",
                mx - 1, my_top, 
                mx + 1, my_top, 
                mx + 1, my_bot, 
                mx - 1, my_bot
            )
            table.insert(ass_lines, mark_ass)
        end
    end

    ov.data = table.concat(ass_lines, "\n")
    ov:update()
end

-- ==========================================
-- 4. Mouse Click Logic (Seek Logic)
-- ==========================================
function on_mouse_click()
    -- 1. Reset activity on every click
    reset_activity()

    local mx, my = mp.get_mouse_pos()
    if not mx or not my then return end -- Prevent error if mouse is outside window

    local w, h = mp.get_osd_size()
    
    -- 2. Reconstruct geometry (sync with draw_ui)
    local bar_y = h - BAR_MARGIN_Y
    local bar_x = BAR_MARGIN_X
    local bar_w = w - (BAR_MARGIN_X * 2)
    
    -- 3. Y-axis Hitbox check
    -- Check if mouse is within the progress bar's Y-range (including HITBOX_SIZE tolerance)
    if my >= (bar_y - HITBOX_SIZE) and my <= (bar_y + BAR_HEIGHT + HITBOX_SIZE) then
        
        -- 4. X-axis calculation (relative position)
        local click_relative_x = mx - bar_x
        local click_pct = click_relative_x / bar_w
        
        -- 5. Boundary check and seek
        if click_pct >= 0 and click_pct <= 1 then
            -- Seek to absolute percentage
            mp.commandv("seek", click_pct * 100, "absolute-percent")
            
            -- [Optional] Show OSD message for feedback
            mp.osd_message(string.format("Seek: %d%%", click_pct * 100))
        end
    else
        -- If click is not on progress bar, toggle pause
        mp.command("cycle pause")
        mp.osd_message("Pause/Play", 0.5) -- Brief OSD message
    end
end

-- ==========================================
-- 5. Event bindings
-- ==========================================
mp.register_script_message("update_markers", function(json)
    local s, d = pcall(utils.parse_json, json)
    if s and d then markers = d; draw_ui() end
end)

-- Refactor jump logic into standalone function
local function jump_next_marker()
    mp.msg.info("Executing jump_next_marker...")
    if not markers or #markers == 0 then
        mp.osd_message("No markers")
        return
    end

    local pos = mp.get_property_number("time-pos") or 0
    
    -- Ensure numeric sorting
    table.sort(markers, function(a, b) return tonumber(a) < tonumber(b) end)

    for _, m in ipairs(markers) do
        local val = tonumber(m)
        -- Threshold of 0.01s
        if val and val > (pos + 0.01) then
            mp.set_property_number("time-pos", val)
            mp.osd_message("Jump: " .. string.format("%.1f", val))
            return
        end
    end
    mp.osd_message("No next marker")
end

mp.add_key_binding("n", "jump_next", jump_next_marker)
mp.register_script_message("jump_next_marker", jump_next_marker)

local function send_current_time()
    local pos = mp.get_property_number("time-pos") or 0
    mp.commandv("script-message", "rust_add_marker", tostring(pos))
    mp.osd_message("Marked: " .. string.format("%.1f", pos))
end

mp.add_key_binding("m", "send_time", send_current_time)
mp.register_script_message("trigger_marker_send", send_current_time)

-- Bind MBTN_LEFT to our seek function (forced)
mp.add_forced_key_binding("MBTN_LEFT", "click_seek", function()
    on_mouse_click()
end)

mp.observe_property("mouse-pos", "native", reset_activity)
mp.observe_property("time-pos", "number", draw_ui)
mp.observe_property("osd-dimensions", "native", draw_ui)

reset_activity()
