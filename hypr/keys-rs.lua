-- What opens Oxy (Rust core) — the experimental daemon-backed build.
--
-- It installs beside the stock oxy-keys.lua, on its own key, so both
-- launchers live on the same machine and comparing them is one keystroke
-- apart: Super+K asks the scripts, Super+R asks oxyd.
--
-- The key is a line you edit, like the main preset: nothing reads a setting.

local key = "SUPER + R"

-- Whatever held R before steps aside, the same way the main preset takes K.
hl.unbind("SUPER + R")
o.bind(key, "Oxy (Rust)", "omarchy-shell shell toggle oma.oxyrs")
