-- Summarises each full scan, and refuses to delete Git repositories' .git
-- directories, which are easy to mark by accident and hard to recover.

local GB = 1000 * 1000 * 1000

return {
  on_scan_complete = function(self, hook, ctx)
    gf.notify(string.format(
      "Scanned %d files, %.1f GB",
      hook.total_files,
      hook.total_size / GB
    ))
  end,

  on_delete_start = function(self, hook, ctx)
    for _, path in ipairs(hook.items) do
      if path:match("/%.git$") then
        gf.notify("Not deleting " .. path .. ": it is a Git repository's history", "error")
        return { prevent_default = true }
      end
    end
  end,
}
