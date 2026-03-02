// Large File Warning Plugin
// Logs a warning if a file over 1GB is seen during a scan.

const GB = 1024 * 1024 * 1024;

fn on_scan_progress(hook) {
    let tree = hook.tree;
    
    // Check if the current scanned node is a large file
    if tree.root.size > GB && !tree.root.is_dir() {
        gf::log_warn(`Large file detected: ${tree.root.name} (${tree.root.size} bytes)`);
    }
    
    // Prevent default propagation if we want (optional)
    return #{
        prevent_default: false,
        stop_propagation: false
    };
}
