if vim.g.loaded_redline then
  return
end
vim.g.loaded_redline = true

local subs = { 'attach', 'detach', 'threads', 'thread', 'accept', 'modify', 'reject', 'comment', 'delete', 'edit', 'reply', 'resolve', 'next', 'prev', 'history', 'refresh', 'help', 'style' }

vim.api.nvim_create_user_command('Redline', function(o)
  local r = require('redline')
  local sub = o.fargs[1] or 'attach'
  if not r[sub] or not vim.tbl_contains(subs, sub) then
    return vim.notify('redline: no command "' .. sub .. '"', vim.log.levels.ERROR)
  end
  if (sub == 'comment' or sub == 'delete' or sub == 'edit') and o.range > 0 then
    return r[sub]({ line1 = o.line1, line2 = o.line2 })
  end
  r[sub](tonumber(o.fargs[2]) or o.fargs[2])
end, {
  nargs = '*',
  range = true,
  desc = 'redline: review agent proposals and comments on this document',
  complete = function(lead)
    return vim.tbl_filter(function(s) return s:find(lead, 1, true) == 1 end, subs)
  end,
})
