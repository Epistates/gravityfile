-- Git Notifier Plugin
-- Notifies the user when they navigate into a directory with untracked files.

function on_navigate(self, hook)
    local node = hook.node
    local count = 0
    
    -- Check if the directory itself has a git status (modified/untracked)
    if node.git_status == "untracked" then
        gf.notify("Warning: Navigated into untracked directory!", "warn")
    end
    
    -- Iterate through children to count untracked files
    for i, child in ipairs(node.children) do
        if child.git_status == "untracked" then
            count = count + 1
        end
    end
    
    if count > 5 then
        gf.notify(string.format("This directory contains %d untracked files.", count), "info")
    end
end

return {
    on_navigate = on_navigate
}
