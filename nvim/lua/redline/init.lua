-- redline for Neovim. A thin layer on the `redline` CLI: the CLI holds the
-- event log and finds the anchors; this file draws them and maps keys.
local M = {}
local api = vim.api
local ns = api.nvim_create_namespace('redline')
local focus_ns = api.nvim_create_namespace('redline_focus')

M.config = {
  bin = 'redline',
  author = nil, -- name in the history; default: the CLI config, then $USER
  keys = true, -- set buffer-local keymaps on attach
  prefix = '<leader>r',
  list_width = 56,
  -- How a comment shows its bounds: 'bg' (background), 'brackets' (virtual
  -- { and }), or 'carets' (a virtual line of ^ below the text).
  comment_style = 'bg',
}

-- per document buffer: { file, threads, watch, timer }
local docs = {}
local ICON = { comment = '●', edit = '~', insert = '+', delete = '-' }

function M.setup(opts)
  M.config = vim.tbl_extend('force', M.config, opts or {})
end

local function hl()
  local set = function(name, v)
    v.default = true
    api.nvim_set_hl(0, name, v)
  end
  set('RedlineAdd', { fg = '#82d68c' })
  set('RedlineDelete', { fg = '#f0787c', strikethrough = true })
  set('RedlineComment', { bg = '#3a3218' })
  set('RedlineSignComment', { fg = '#f0c674' })
  set('RedlineSignEdit', { fg = '#ffaa6e' })
  set('RedlineSignInsert', { fg = '#82d68c' })
  set('RedlineSignDelete', { fg = '#f0787c' })
  set('RedlineMeta', { link = 'Comment' })
  set('RedlineBound', { fg = '#f0c674', bold = true })
  set('RedlineFocus', { bg = '#46557f', bold = true })
  set('RedlineFocusLine', { bg = '#262b3d' })
end

local function buf_text(buf)
  return table.concat(api.nvim_buf_get_lines(buf, 0, -1, false), '\n') .. '\n'
end

-- Run the CLI as the human. Returns stdout, or nil after an error.
local function cli(args, stdin)
  local cmd = { M.config.bin }
  vim.list_extend(cmd, args)
  local env = { REDLINE_ROLE = 'human' }
  if M.config.author then
    env.REDLINE_AUTHOR = M.config.author
  end
  local ok, res = pcall(function()
    return vim.system(cmd, { stdin = stdin, text = true, env = env }):wait()
  end)
  if not ok then
    vim.notify('redline: cannot run `' .. M.config.bin .. '`. Is it on $PATH?', vim.log.levels.ERROR)
    return nil
  end
  if res.code ~= 0 then
    vim.notify(vim.trim(res.stderr or ''), vim.log.levels.ERROR)
    return nil
  end
  return res.stdout
end

-- The document buffer for the current buffer (a list or thread buffer points at one).
local function doc_buf()
  local buf = api.nvim_get_current_buf()
  local src = vim.b[buf].redline_doc or buf
  if docs[src] then
    return src
  end
  vim.notify('redline: this buffer is not attached. Run :Redline', vim.log.levels.WARN)
end

local function has_range(t)
  return type(t.range) == 'table'
end

-- ── drawing ───────────────────────────────────────────────────────────────

local function render(buf)
  local d = docs[buf]
  api.nvim_buf_clear_namespace(buf, ns, 0, -1)
  local last = api.nvim_buf_line_count(buf) - 1
  for _, t in ipairs(d.threads) do
    if has_range(t) then
      local s, e = t.range.start, t.range['end']
      local sign = { sign_text = ICON[t.kind], sign_hl_group = 'RedlineSign' .. t.kind:sub(1, 1):upper() .. t.kind:sub(2) }
      local meta = { { ('  #%d %s'):format(t.id, t.author), 'RedlineMeta' } }
      local new = vim.split(t.new or '', '\n', { plain = true })
      pcall(function()
        if t.kind == 'insert' then
          local vl = {}
          if not t.tight then
            vl[#vl + 1] = { { '', 'RedlineAdd' } }
          end
          for _, l in ipairs(new) do
            vl[#vl + 1] = { { l, 'RedlineAdd' } }
          end
          local top = t.quote == ''
          api.nvim_buf_set_extmark(buf, ns, math.min(e.line, last), 0, vim.tbl_extend('force', sign, {
            virt_lines = vl,
            virt_lines_above = top,
            virt_text = meta,
            virt_text_pos = 'eol',
          }))
        else
          local style = t.kind == 'comment' and M.config.comment_style or 'bg'
          local mark = function(row, col, opts)
            api.nvim_buf_set_extmark(buf, ns, row, col, opts)
          end
          if style == 'brackets' then
            -- Virtual braces around the text.
            mark(s.line, s.col, vim.tbl_extend('force', sign, {
              virt_text = { { '{', 'RedlineBound' } },
              virt_text_pos = 'inline',
              right_gravity = false,
            }))
            mark(e.line, e.col, { virt_text = { { '}', 'RedlineBound' } }, virt_text_pos = 'inline' })
            mark(s.line, 0, { virt_text = meta, virt_text_pos = 'eol' })
          elseif style == 'carets' then
            -- A virtual line of ^ below the text, like a compiler message.
            -- A range of more than one line also gets a line of v above it.
            local w = vim.fn.strdisplaywidth
            local first = api.nvim_buf_get_lines(buf, s.line, s.line + 1, false)[1] or ''
            local last = api.nvim_buf_get_lines(buf, e.line, e.line + 1, false)[1] or ''
            local gist = vim.split(t.body or '', '\n', { plain = true })[1]
            local label = { (' #%d %s: %s'):format(t.id, t.author, gist), 'RedlineMeta' }
            local pad = (' '):rep(w(first:sub(1, s.col)))
            if s.line == e.line then
              local n = math.max(1, w(first:sub(s.col + 1, e.col)))
              mark(s.line, 0, vim.tbl_extend('force', sign, {
                virt_lines = { { { pad .. ('^'):rep(n), 'RedlineBound' }, label } },
              }))
            else
              local n = math.max(1, w(first:sub(s.col + 1)))
              mark(s.line, 0, vim.tbl_extend('force', sign, {
                virt_lines = { { { pad .. ('v'):rep(n), 'RedlineBound' } } },
                virt_lines_above = true,
              }))
              mark(e.line, 0, {
                virt_lines = { { { ('^'):rep(math.max(1, w(last:sub(1, e.col)))), 'RedlineBound' }, label } },
              })
            end
          else
            local group = t.kind == 'comment' and 'RedlineComment' or 'RedlineDelete'
            mark(s.line, s.col, vim.tbl_extend('force', sign, {
              end_row = e.line,
              end_col = e.col,
              hl_group = group,
              virt_text = meta,
              virt_text_pos = 'eol',
            }))
          end
          if t.kind == 'edit' then
            local vl = {}
            for i = 2, #new do
              vl[#vl + 1] = { { new[i], 'RedlineAdd' } }
            end
            api.nvim_buf_set_extmark(buf, ns, e.line, e.col, {
              virt_text = { { new[1], 'RedlineAdd' } },
              virt_text_pos = 'inline',
              virt_lines = #vl > 0 and vl or nil,
            })
          end
        end
      end)
    end
  end
end

local render_list -- defined below

function M.refresh()
  local buf = doc_buf()
  if not buf or not api.nvim_buf_is_valid(buf) then
    return
  end
  local d = docs[buf]
  local out = cli({ 'threads', d.file, '--json', '--stdin' }, buf_text(buf))
  if not out then
    return
  end
  d.threads = vim.json.decode(out)
  render(buf)
  render_list(buf)
end

local function refresh_buf(buf)
  if docs[buf] and api.nvim_buf_is_valid(buf) then
    api.nvim_buf_call(buf, M.refresh)
  end
end

local function soon(buf)
  local d = docs[buf]
  if d.timer then
    d.timer:stop()
  end
  d.timer = vim.defer_fn(function()
    refresh_buf(buf)
  end, 250)
end

-- ── threads ───────────────────────────────────────────────────────────────

local function find(buf, id)
  for _, t in ipairs(docs[buf].threads) do
    if t.id == id then
      return t
    end
  end
end

-- The thread for a command: the given id, the thread of a thread buffer, the
-- thread on the list line, or the thread at the cursor in the document.
local function pick(buf, id)
  local cur = api.nvim_get_current_buf()
  id = id or vim.b[cur].redline_thread
  if not id and vim.b[cur].redline_list then
    id = tonumber(api.nvim_get_current_line():match('^#(%d+)'))
  end
  if id then
    local t = find(buf, id)
    if not t then
      vim.notify('redline: no open thread #' .. id, vim.log.levels.WARN)
    end
    return t
  end
  local row, col = unpack(api.nvim_win_get_cursor(0))
  row = row - 1
  local best
  for _, t in ipairs(docs[buf].threads) do
    if has_range(t) then
      local s, e = t.range.start, t.range['end']
      if row >= s.line and row <= e.line then
        local inside = (row > s.line or col >= s.col) and (row < e.line or col <= e.col)
        if inside or not best then
          best = t
        end
      end
    end
  end
  if not best then
    vim.notify('redline: no thread here. ]r goes to the next thread', vim.log.levels.INFO)
  end
  return best
end

local function doc_win(buf)
  for _, w in ipairs(api.nvim_list_wins()) do
    if api.nvim_win_get_buf(w) == buf then
      return w
    end
  end
end

-- Mark one thread in the document and scroll the document window to it.
-- The cursor stays where it is. `nil` removes the mark.
local function focus(buf, t)
  if not api.nvim_buf_is_valid(buf) then
    return
  end
  api.nvim_buf_clear_namespace(buf, focus_ns, 0, -1)
  if not t or not has_range(t) then
    return
  end
  local s, e = t.range.start, t.range['end']
  pcall(function()
    for row = s.line, e.line do
      api.nvim_buf_set_extmark(buf, focus_ns, row, 0, { line_hl_group = 'RedlineFocusLine', priority = 190 })
    end
    if e.line > s.line or e.col > s.col then
      api.nvim_buf_set_extmark(buf, focus_ns, s.line, s.col, {
        end_row = e.line,
        end_col = e.col,
        hl_group = 'RedlineFocus',
        priority = 200,
      })
    end
  end)
  local win = doc_win(buf)
  if win and win ~= api.nvim_get_current_win() then
    pcall(api.nvim_win_set_cursor, win, { s.line + 1, s.col })
    api.nvim_win_call(win, function()
      vim.cmd('normal! zz')
    end)
  end
end

local function jump(buf, t)
  if not has_range(t) then
    return vim.notify('redline: the anchor text of #' .. t.id .. ' is gone', vim.log.levels.WARN)
  end
  local win = doc_win(buf)
  if win then
    api.nvim_set_current_win(win)
  else
    api.nvim_set_current_buf(buf)
  end
  api.nvim_win_set_cursor(0, { t.range.start.line + 1, t.range.start.col })
  vim.cmd('normal! zz')
end

local function step(dir)
  local buf = doc_buf()
  if not buf then
    return
  end
  local row, col = unpack(api.nvim_win_get_cursor(0))
  local pos = (row - 1) * 100000 + col
  local list = vim.tbl_filter(has_range, docs[buf].threads)
  table.sort(list, function(a, b)
    return a.range.start.line * 100000 + a.range.start.col < b.range.start.line * 100000 + b.range.start.col
  end)
  if #list == 0 then
    return vim.notify('redline: no open threads', vim.log.levels.INFO)
  end
  local target
  if dir > 0 then
    for _, t in ipairs(list) do
      if t.range.start.line * 100000 + t.range.start.col > pos then
        target = t
        break
      end
    end
    target = target or list[1]
  else
    for i = #list, 1, -1 do
      local t = list[i]
      if t.range.start.line * 100000 + t.range.start.col < pos then
        target = t
        break
      end
    end
    target = target or list[#list]
  end
  jump(buf, target)
end

function M.next()
  step(1)
end
function M.prev()
  step(-1)
end

-- A small scratch buffer to write text in. :w sends the text, q cancels.
local function compose(title, initial, cb)
  vim.cmd('botright 8new')
  local b = api.nvim_get_current_buf()
  api.nvim_buf_set_name(b, ('redline://write/%d/%s'):format(b, title:gsub('[^%w#]+', '-')))
  vim.bo[b].buftype = 'acwrite'
  vim.bo[b].bufhidden = 'wipe'
  vim.bo[b].swapfile = false
  vim.bo[b].filetype = 'markdown'
  api.nvim_buf_set_lines(b, 0, -1, false, vim.split(initial or '', '\n', { plain = true }))
  vim.bo[b].modified = false
  vim.wo.winbar = ' redline · ' .. title .. ' · :w sends · q cancels'
  api.nvim_create_autocmd('BufWriteCmd', {
    buffer = b,
    callback = function()
      local text = vim.trim(table.concat(api.nvim_buf_get_lines(b, 0, -1, false), '\n'))
      vim.bo[b].modified = false
      vim.cmd('bwipeout! ' .. b)
      cb(text)
    end,
  })
  vim.keymap.set('n', 'q', '<cmd>bwipeout!<cr>', { buffer = b, nowait = true })
  if (initial or '') == '' then
    vim.cmd('startinsert')
  end
end

local function after(buf)
  refresh_buf(buf)
  local cur = api.nvim_get_current_buf()
  if vim.b[cur].redline_thread then
    M.thread(vim.b[cur].redline_thread)
  end
end

function M.accept(id, new)
  local buf = doc_buf()
  local t = buf and pick(buf, id)
  if not t then
    return
  end
  if t.kind == 'comment' then
    return vim.notify('redline: #' .. t.id .. ' is a comment. Resolve it', vim.log.levels.INFO)
  end
  local args = { 'accept', docs[buf].file, tostring(t.id), '--json', '--stdin' }
  if new then
    vim.list_extend(args, { '--new', new })
  end
  local out = cli(args, buf_text(buf))
  if not out then
    return
  end
  local e = vim.json.decode(out).edit
  if type(e) == 'table' then
    -- The CLI gives the edit; the buffer applies it, so `u` undoes it.
    api.nvim_buf_set_lines(buf, e.line, e.line + e.remove, false, e.insert)
  end
  vim.notify(('redline: accepted #%d. Save the buffer to record the edit'):format(t.id))
  after(buf)
end

function M.modify(id)
  local buf = doc_buf()
  local t = buf and pick(buf, id)
  if not t then
    return
  end
  if t.kind ~= 'edit' and t.kind ~= 'insert' then
    return vim.notify('redline: only an edit or an insert has text to change', vim.log.levels.INFO)
  end
  compose(('change #%d, then accept'):format(t.id), t.new, function(text)
    if text ~= '' then
      api.nvim_buf_call(buf, function()
        M.accept(t.id, text)
      end)
    end
  end)
end

function M.reject(id)
  local buf = doc_buf()
  local t = buf and pick(buf, id)
  if not t then
    return
  end
  if t.kind == 'comment' then
    return vim.notify('redline: #' .. t.id .. ' is a comment. Resolve it', vim.log.levels.INFO)
  end
  compose(('reject #%d: reason (optional)'):format(t.id), '', function(text)
    local args = { 'reject', docs[buf].file, tostring(t.id) }
    if text ~= '' then
      vim.list_extend(args, { '--note', text })
    end
    if cli(args) then
      vim.notify('redline: rejected #' .. t.id)
      after(buf)
    end
  end)
end

function M.resolve(id)
  local buf = doc_buf()
  local t = buf and pick(buf, id)
  if t and cli({ 'resolve', docs[buf].file, tostring(t.id) }) then
    vim.notify('redline: resolved #' .. t.id)
    after(buf)
  end
end

function M.reply(id)
  local buf = doc_buf()
  local t = buf and pick(buf, id)
  if not t then
    return
  end
  compose('reply on #' .. t.id, '', function(text)
    if text ~= '' and cli({ 'reply', docs[buf].file, tostring(t.id), '--body', text }) then
      after(buf)
    end
  end)
end

-- ── operators: comment, delete and edit take a motion ─────────────────────

-- The text that a command points at: { text, line1, line2, linewise }.
-- `kind` is 'line', 'char' or 'block' (from an operator), or a visual mode.
local function region(p1, p2, kind)
  local linewise = kind == 'line' or kind == 'V'
  if p1[2] > p2[2] or (p1[2] == p2[2] and p1[3] > p2[3]) then
    p1, p2 = p2, p1
  end
  local lines = vim.fn.getregion(p1, p2, { type = linewise and 'V' or 'v' })
  return { text = table.concat(lines, '\n'), line1 = p1[2], line2 = p2[2], linewise = linewise }
end

local function line_region(l1, l2)
  local lines = api.nvim_buf_get_lines(0, l1 - 1, l2, false)
  return { text = table.concat(lines, '\n'), line1 = l1, line2 = l2, linewise = true }
end

-- CLI arguments that point at a region.
local function target_args(r)
  if r.linewise then
    return { '--lines', r.line1 == r.line2 and tostring(r.line1) or (r.line1 .. '-' .. r.line2) }
  end
  return { '--text', r.text, '--near-line', tostring(r.line1) }
end

local function where(r)
  return 'L' .. (r.line1 == r.line2 and r.line1 or (r.line1 .. '–' .. r.line2))
end

local function comment_region(buf, r)
  compose('comment on ' .. where(r), '', function(text)
    if text == '' then
      return
    end
    local args = { 'comment', docs[buf].file, '--body', text, '--stdin' }
    vim.list_extend(args, target_args(r))
    if cli(args, buf_text(buf)) then
      after(buf)
    end
  end)
end

-- Suggest that the text goes away. The text stays until someone accepts.
local function delete_region(buf, r)
  local args = { 'propose', 'delete', docs[buf].file, '--stdin' }
  vim.list_extend(args, target_args(r))
  if cli(args, buf_text(buf)) then
    vim.notify('redline: suggested a delete on ' .. where(r))
    after(buf)
  end
end

-- Suggest a replacement. The buffer starts with the current text.
local function edit_region(buf, r)
  compose('suggest an edit on ' .. where(r), r.text, function(text)
    if text == '' or text == vim.trim(r.text) then
      return vim.notify('redline: no change: nothing suggested', vim.log.levels.INFO)
    end
    local args = { 'propose', 'edit', docs[buf].file, '--new', text, '--stdin' }
    vim.list_extend(args, target_args(r))
    if cli(args, buf_text(buf)) then
      vim.notify('redline: suggested an edit on ' .. where(r))
      after(buf)
    end
  end)
end

local ACTIONS = { comment = comment_region, delete = delete_region, edit = edit_region }
local pending_action

-- 'operatorfunc' target. Neovim calls it after the motion.
function M._operator(kind)
  local buf = doc_buf()
  if buf and pending_action then
    ACTIONS[pending_action](buf, region(vim.fn.getpos("'["), vim.fn.getpos("']"), kind))
  end
end

-- The keys that start an operator: use it in an `expr` mapping.
-- `line = true` gives the doubled form that acts on the current line(s).
function M.operator(action, line)
  pending_action = action
  vim.o.operatorfunc = "v:lua.require'redline'._operator"
  return line and 'g@_' or 'g@'
end

local function act_on_range(action, range)
  local buf = doc_buf()
  if not buf then
    return
  end
  range = type(range) == 'table' and range or nil
  local l1 = range and range.line1 or api.nvim_win_get_cursor(0)[1]
  ACTIONS[action](buf, line_region(l1, range and range.line2 or l1))
end

-- Comment on / suggest a delete of / suggest an edit of the current line,
-- or of lines range.line1..range.line2.
function M.comment(range)
  act_on_range('comment', range)
end
function M.delete(range)
  act_on_range('delete', range)
end
function M.edit(range)
  act_on_range('edit', range)
end

-- ── thread list buffer ────────────────────────────────────────────────────

local function scratch(name, doc)
  local b = vim.fn.bufnr(name)
  if b == -1 then
    b = api.nvim_create_buf(false, true)
    api.nvim_buf_set_name(b, name)
    vim.bo[b].buftype = 'nofile'
    vim.bo[b].swapfile = false
    vim.b[b].redline_doc = doc
  end
  return b
end

local function set_lines(b, lines)
  vim.bo[b].modifiable = true
  api.nvim_buf_set_lines(b, 0, -1, false, lines)
  vim.bo[b].modifiable = false
  vim.bo[b].modified = false
end

local function act_keys(b)
  local map = function(lhs, fn, desc)
    vim.keymap.set('n', lhs, fn, { buffer = b, nowait = true, desc = 'redline: ' .. desc })
  end
  map('a', function() M.accept() end, 'accept')
  map('x', function() M.reject() end, 'reject')
  map('m', function() M.modify() end, 'modify, then accept')
  map('r', function() M.reply() end, 'reply')
  map('R', function() M.resolve() end, 'resolve')
  map('q', '<cmd>close<cr>', 'close')
  map('g?', M.help, 'show the keys')
end

render_list = function(buf)
  local name = 'redline://threads/' .. docs[buf].file
  local b = vim.fn.bufnr(name)
  if b == -1 then
    return
  end
  local lines = {}
  for _, t in ipairs(docs[buf].threads) do
    local place = has_range(t) and ('L' .. (t.range.start.line + 1)) or 'lost'
    local gist = t.kind == 'comment' and t.body or (t.kind == 'delete' and t.quote or t.new)
    gist = vim.split(gist, '\n', { plain = true })[1]
    lines[#lines + 1] = ('#%-3d %s %-7s %-10s %-5s %s'):format(t.id, ICON[t.kind], t.kind, t.author, place, gist)
  end
  if #lines == 0 then
    lines = { 'No open threads.' }
  end
  set_lines(b, lines)
end

function M.threads()
  local buf = doc_buf()
  if not buf then
    return
  end
  local name = 'redline://threads/' .. docs[buf].file
  local b = scratch(name, buf)
  vim.b[b].redline_list = true
  local win = vim.fn.bufwinid(b)
  if win ~= -1 then
    api.nvim_set_current_win(win)
  else
    vim.cmd('botright vertical ' .. M.config.list_width .. 'split')
    api.nvim_win_set_buf(0, b)
    vim.wo.wrap = false
    vim.wo.cursorline = true
    vim.wo.winbar = ' redline threads · ⏎ open · gd go to text · a x m r R · q · g? keys'
  end
  act_keys(b)
  local open = function()
    local t = pick(buf)
    if t then
      M.thread(t.id)
    end
  end
  vim.keymap.set('n', '<cr>', open, { buffer = b, nowait = true, desc = 'redline: open the thread as a buffer' })
  vim.keymap.set('n', 'o', open, { buffer = b, nowait = true, desc = 'redline: open the thread as a buffer' })
  vim.keymap.set('n', 'gd', function()
    local t = pick(buf)
    if t then
      jump(buf, t)
    end
  end, { buffer = b, desc = 'redline: go to the thread in the document' })
  -- The thread on the cursor line shows as a highlight in the document.
  local group = api.nvim_create_augroup('redline_list_' .. b, { clear = true })
  api.nvim_create_autocmd({ 'CursorMoved', 'BufEnter' }, {
    group = group,
    buffer = b,
    callback = function()
      if docs[buf] then
        local id = tonumber(api.nvim_get_current_line():match('^#(%d+)'))
        focus(buf, id and find(buf, id))
      end
    end,
  })
  api.nvim_create_autocmd({ 'BufLeave', 'BufWipeout' }, {
    group = group,
    buffer = b,
    callback = function()
      focus(buf, nil)
    end,
  })
  render_list(buf)
end

-- ── thread buffer ─────────────────────────────────────────────────────────

local MARK = '--- write a reply below this line, then :w ---'

-- Open one thread as a buffer. Text below the marker is a reply: :w sends it.
function M.thread(id)
  local buf = doc_buf()
  if not buf then
    return
  end
  local d = docs[buf]
  if not id then
    local t = pick(buf)
    if not t then
      return
    end
    id = t.id
  end
  local out = cli({ 'thread', d.file, tostring(id), '--json', '--stdin' }, buf_text(buf))
  if not out then
    return
  end
  local t = vim.json.decode(out)
  local place = has_range(t) and ('L' .. (t.range.start.line + 1)) or ''
  local L = { ('# #%d %s · %s · %s %s'):format(t.id, t.kind, t.author, t.status, place), '' }
  local add = function(prefix, text)
    for _, l in ipairs(vim.split(text, '\n', { plain = true })) do
      L[#L + 1] = prefix .. l
    end
  end
  if t.kind == 'comment' then
    add('> ', t.quote)
  else
    L[#L + 1] = '```diff'
    if t.kind ~= 'insert' then
      add('- ', t.quote)
    end
    if t.kind ~= 'delete' then
      add('+ ', t.new)
    end
    L[#L + 1] = '```'
  end
  L[#L + 1] = ''
  if t.body ~= '' then
    add('', t.body)
    L[#L + 1] = ''
  end
  for _, r in ipairs(t.replies) do
    add('', ('**%s**: %s'):format(r.author, r.body))
    L[#L + 1] = ''
  end
  if type(t.note) == 'string' then
    add('', 'Decision note: ' .. t.note)
    L[#L + 1] = ''
  end
  if t.status == 'open' then
    L[#L + 1] = MARK
    L[#L + 1] = ''
  end

  local name = ('redline://thread/%d/%s'):format(id, d.file)
  local b = vim.fn.bufnr(name)
  if b == -1 then
    b = api.nvim_create_buf(false, true)
    api.nvim_buf_set_name(b, name)
    vim.bo[b].buftype = 'acwrite'
    vim.bo[b].swapfile = false
    vim.bo[b].filetype = 'markdown'
    vim.b[b].redline_doc = buf
    vim.b[b].redline_thread = id
    act_keys(b)
    local go = function()
      local th = find(buf, id)
      if th then
        jump(buf, th)
      end
    end
    vim.keymap.set('n', '<cr>', go, { buffer = b, desc = 'redline: go to the thread in the document' })
    vim.keymap.set('n', 'gd', go, { buffer = b, desc = 'redline: go to the thread in the document' })
    -- q goes back to the thread list when there is one.
    vim.keymap.set('n', 'q', function()
      local list = vim.fn.bufnr('redline://threads/' .. d.file)
      if list ~= -1 and vim.fn.bufwinid(list) == -1 then
        api.nvim_win_set_buf(0, list)
      else
        vim.cmd('close')
      end
    end, { buffer = b, nowait = true, desc = 'redline: back to the thread list' })
    api.nvim_create_autocmd('BufEnter', {
      buffer = b,
      callback = function()
        if docs[buf] then
          focus(buf, find(buf, id))
        end
      end,
    })
    api.nvim_create_autocmd({ 'BufLeave', 'BufWipeout' }, {
      buffer = b,
      callback = function()
        focus(buf, nil)
      end,
    })
    api.nvim_create_autocmd('BufWriteCmd', {
      buffer = b,
      callback = function()
        local lines = api.nvim_buf_get_lines(b, 0, -1, false)
        local reply = {}
        for i = #lines, 1, -1 do
          if lines[i] == MARK then
            break
          end
          table.insert(reply, 1, lines[i])
        end
        local text = vim.trim(table.concat(reply, '\n'))
        if text == '' then
          vim.bo[b].modified = false
          return vim.notify('redline: write the reply below the marker line', vim.log.levels.INFO)
        end
        if cli({ 'reply', d.file, tostring(id), '--body', text }) then
          refresh_buf(buf)
          M.thread(id)
        end
      end,
    })
  end
  if api.nvim_get_current_buf() ~= b then
    -- From the document, use the list window or a new split; from the list, replace the list.
    if api.nvim_get_current_buf() == buf then
      vim.cmd('botright vertical ' .. M.config.list_width .. 'split')
    end
    api.nvim_win_set_buf(0, b)
  end
  vim.bo[b].modifiable = true
  api.nvim_buf_set_lines(b, 0, -1, false, L)
  vim.bo[b].modified = false
  vim.wo.wrap = true
  vim.wo.winbar = ' redline thread · a x m R decide · ⏎ go to text · :w sends the reply · q back · g? keys'
  api.nvim_win_set_cursor(0, { #L, 0 })
  focus(buf, find(buf, id))
end

function M.history()
  local buf = doc_buf()
  if not buf then
    return
  end
  local out = cli({ 'log', docs[buf].file })
  if not out then
    return
  end
  local b = scratch('redline://history/' .. docs[buf].file, buf)
  vim.cmd('botright split')
  api.nvim_win_set_buf(0, b)
  set_lines(b, vim.split(vim.trim(out), '\n', { plain = true }))
  vim.keymap.set('n', 'q', '<cmd>close<cr>', { buffer = b, nowait = true })
  api.nvim_win_set_cursor(0, { api.nvim_buf_line_count(b), 0 })
end

-- Set how comments show their bounds: 'bg', 'brackets' or 'carets'. With no
-- name, go to the next style.
function M.style(name)
  local styles = { 'bg', 'brackets', 'carets' }
  if not vim.tbl_contains(styles, name) then
    local i = vim.fn.index(styles, M.config.comment_style)
    name = styles[(i + 1) % #styles + 1]
  end
  M.config.comment_style = name
  for buf in pairs(docs) do
    if api.nvim_buf_is_valid(buf) then
      render(buf)
    end
  end
  vim.notify('redline: comment style: ' .. name)
end

-- ── help ─────────────────────────────────────────────────────────────────

-- Show the key bindings for the current buffer in a floating window.
function M.help()
  local cur = api.nvim_get_current_buf()
  local p = M.config.prefix
  local rows
  if vim.b[cur].redline_list then
    rows = {
      { 'THREAD LIST' },
      { 'j  k', 'move: the thread on the cursor line shows in the document' },
      { '<cr>  o', 'open the thread as a buffer' },
      { 'gd', 'go to the thread in the document' },
      { 'a', 'accept the proposal' },
      { 'm', 'modify the proposal, then accept' },
      { 'x', 'reject the proposal, with a reason' },
      { 'r', 'reply' },
      { 'R', 'resolve the comment' },
      { 'q', 'close the list' },
    }
  elseif vim.b[cur].redline_thread then
    rows = {
      { 'THREAD' },
      { ':w', 'send the text below the marker line as a reply' },
      { '<cr>  gd', 'go to the thread in the document' },
      { 'a', 'accept the proposal' },
      { 'm', 'modify the proposal, then accept' },
      { 'x', 'reject the proposal, with a reason' },
      { 'r', 'reply in a small buffer' },
      { 'R', 'resolve the comment' },
      { 'q', 'back to the thread list' },
    }
  else
    rows = {
      { 'MOVE' },
      { ']r  [r', 'next / previous thread' },
      { 'REVIEW' },
      { p .. 'a', 'accept the proposal at the cursor' },
      { p .. 'm', 'modify the proposal, then accept' },
      { p .. 'x', 'reject the proposal, with a reason' },
      { p .. 'c{motion}', 'comment on the text of the motion' },
      { p .. 'd{motion}', 'suggest a delete of that text' },
      { p .. 'e{motion}', 'suggest an edit of that text' },
      { p .. 'cc  dd  ee', 'the same, for the line (a count works)' },
      { '{visual}' .. p .. 'c d e', 'the same, for the selection' },
      { p .. 'r', 'reply on the thread' },
      { p .. 'R', 'resolve the comment' },
      { 'THREADS' },
      { p .. 't', 'thread list, as a buffer' },
      { p .. 'o', 'open the thread at the cursor as a buffer' },
      { p .. 'h', 'history of events' },
      { ':Redline style', 'next comment style: bg, brackets, carets' },
      { 'WRITE' },
      { ':w', 'a save records your edit' },
      { 'u', 'undo, also for an accept' },
    }
  end
  rows[#rows + 1] = { 'g?', 'this help' }
  local lines, marks, width = {}, {}, 0
  for _, r in ipairs(rows) do
    local line = r[2] and ('  %-24s %s'):format(r[1], r[2]) or (' ' .. r[1])
    lines[#lines + 1] = line
    marks[#marks + 1] = r[2] and { 'Special', 2, 2 + #r[1] } or { 'Title', 0, #line }
    width = math.max(width, vim.fn.strdisplaywidth(line))
  end
  local b = api.nvim_create_buf(false, true)
  api.nvim_buf_set_lines(b, 0, -1, false, lines)
  for i, m in ipairs(marks) do
    api.nvim_buf_set_extmark(b, ns, i - 1, m[2], { end_col = m[3], hl_group = m[1] })
  end
  vim.bo[b].modifiable = false
  vim.bo[b].bufhidden = 'wipe'
  local win = api.nvim_open_win(b, true, {
    relative = 'editor',
    width = width + 2,
    height = #lines,
    row = math.max(0, math.floor((vim.o.lines - #lines) / 2) - 1),
    col = math.max(0, math.floor((vim.o.columns - width - 2) / 2)),
    style = 'minimal',
    border = 'rounded',
    title = ' redline keys ',
    title_pos = 'center',
  })
  local close = function()
    if api.nvim_win_is_valid(win) then
      api.nvim_win_close(win, true)
    end
  end
  for _, k in ipairs({ 'q', '<esc>', 'g?', '<cr>' }) do
    vim.keymap.set('n', k, close, { buffer = b, nowait = true })
  end
  api.nvim_create_autocmd('BufLeave', { buffer = b, once = true, callback = close })
end

-- ── attach ────────────────────────────────────────────────────────────────

function M.attach()
  local buf = api.nvim_get_current_buf()
  local file = api.nvim_buf_get_name(buf)
  if file == '' or vim.bo[buf].buftype ~= '' then
    return vim.notify('redline: open a file first', vim.log.levels.WARN)
  end
  if docs[buf] then
    return M.refresh()
  end
  if vim.fn.filereadable(file) == 0 then
    vim.cmd('write')
  end
  hl()
  docs[buf] = { file = file, threads = {} }
  local group = api.nvim_create_augroup('redline_' .. buf, { clear = true })
  api.nvim_create_autocmd('BufWritePost', {
    group = group,
    buffer = buf,
    callback = function()
      -- A save is the human's edit. Saves in a row collapse to one event.
      cli({ 'sync', file })
      refresh_buf(buf)
    end,
  })
  api.nvim_create_autocmd({ 'TextChanged', 'InsertLeave' }, {
    group = group,
    buffer = buf,
    callback = function()
      soon(buf)
    end,
  })
  api.nvim_create_autocmd({ 'BufWipeout', 'VimLeavePre' }, {
    group = group,
    buffer = buf,
    callback = function()
      M.detach(buf)
    end,
  })
  -- Live updates: the CLI prints a line for each new event.
  local ok, job = pcall(vim.system, { M.config.bin, 'watch', file }, {
    text = true,
    stdout = function(_, data)
      if data and data ~= '' then
        vim.schedule(function()
          if docs[buf] then
            soon(buf)
          end
        end)
      end
    end,
  })
  docs[buf].watch = ok and job or nil

  if M.config.keys then
    local p = M.config.prefix
    local map = function(mode, lhs, fn, desc)
      vim.keymap.set(mode, lhs, fn, { buffer = buf, desc = 'redline: ' .. desc })
    end
    map('n', ']r', M.next, 'next thread')
    map('n', '[r', M.prev, 'previous thread')
    map('n', p .. 'a', function() M.accept() end, 'accept the proposal')
    map('n', p .. 'm', function() M.modify() end, 'modify the proposal, then accept')
    map('n', p .. 'x', function() M.reject() end, 'reject the proposal')
    -- Operators: prefix + c/d/e, then a motion or a text object. The doubled
    -- key acts on the line. In visual mode they act on the selection.
    for key, action in pairs({ c = 'comment', d = 'delete', e = 'edit' }) do
      local what = action == 'comment' and 'comment on' or ('suggest a ' .. action .. ' of')
      vim.keymap.set('n', p .. key, function()
        return M.operator(action)
      end, { buffer = buf, expr = true, desc = ('redline: %s {motion}'):format(what) })
      vim.keymap.set('n', p .. key .. key, function()
        return M.operator(action, true)
      end, { buffer = buf, expr = true, desc = ('redline: %s the line'):format(what) })
      map('x', p .. key, function()
        local r = region(vim.fn.getpos('v'), vim.fn.getpos('.'), vim.fn.mode())
        api.nvim_feedkeys(api.nvim_replace_termcodes('<esc>', true, false, true), 'nx', false)
        ACTIONS[action](buf, r)
      end, what .. ' the selection')
    end
    map('n', p .. 'r', function() M.reply() end, 'reply on the thread')
    map('n', p .. 'R', function() M.resolve() end, 'resolve the comment')
    map('n', p .. 't', M.threads, 'thread list')
    map('n', p .. 'o', function() M.thread() end, 'open the thread as a buffer')
    map('n', p .. 'h', M.history, 'history')
  end
  vim.keymap.set('n', 'g?', M.help, { buffer = buf, desc = 'redline: show the keys' })
  M.refresh()
end

function M.detach(buf)
  buf = type(buf) == 'number' and buf or api.nvim_get_current_buf()
  local d = docs[buf]
  if not d then
    return
  end
  if d.watch then
    pcall(function()
      d.watch:kill(15)
    end)
  end
  docs[buf] = nil
  pcall(api.nvim_del_augroup_by_name, 'redline_' .. buf)
  if api.nvim_buf_is_valid(buf) then
    api.nvim_buf_clear_namespace(buf, ns, 0, -1)
  end
end

return M
