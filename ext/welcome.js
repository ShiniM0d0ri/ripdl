"use strict";

const pillEl = document.getElementById("state-pill");
const verEl = document.getElementById("ver-label");
const titleEl = document.getElementById("state-title");
const copyEl = document.getElementById("state-copy");
const cmdBox = document.getElementById("cmd-box");
const primaryBtn = document.getElementById("primary-btn");
const retryBtn = document.getElementById("retry-btn");
const optionsBtn = document.getElementById("options-btn");
const copyCmdBtn = document.getElementById("copy-cmd");
const copyCmdLbl = document.getElementById("copy-cmd-lbl");

let currentAction = "close";

async function inspect() {
    pillEl.className = "pill";
    pillEl.textContent = "Checking native bridge…";
    cmdBox.classList.add("hidden");
    try {
        const diag = await chrome.runtime.sendMessage({ type: "ripdl_ext_diagnostics" });
        const extVer = diag?.extVersion || chrome.runtime.getManifest().version;
        const appVer = diag?.appVersion || null;
        verEl.textContent = appVer ? `Extension ${extVer} · app ${appVer}` : `Extension ${extVer}`;

        if (diag?.hostAlive && diag.versionSkew === 0) {
            pillEl.className = "pill ok";
            pillEl.textContent = "Connected & ready";
            titleEl.textContent = "Everything is connected.";
            copyEl.textContent = "ripdl is listening for browser downloads and copied multi-part links. You can close this tab and start downloading.";
            primaryBtn.textContent = "Done";
            currentAction = "close";
        } else if (diag?.hostAlive && diag.versionSkew < 0) {
            pillEl.className = "pill warn";
            pillEl.textContent = "Extension update ready";
            titleEl.textContent = `app ${appVer} is newer than extension ${extVer}`;
            copyEl.textContent = "Click Reload below so the browser switches to the latest bundled extension files.";
            primaryBtn.textContent = "Reload extension";
            currentAction = "reload";
        } else if (diag?.hostAlive && diag.versionSkew > 0) {
            pillEl.className = "pill warn";
            pillEl.textContent = "Desktop app update recommended";
            titleEl.textContent = `Extension ${extVer} is newer than app ${appVer}`;
            copyEl.textContent = "Open ripdl Settings -> Updates or run the latest installer so the desktop daemon matches.";
            primaryBtn.textContent = "Done";
            currentAction = "close";
        } else {
            pillEl.className = "pill bad";
            pillEl.textContent = "Native bridge not responding";
            titleEl.textContent = "Could not reach the desktop ripdl bridge";
            copyEl.textContent = "Launch ripdl or run the native-host registration command once, then click Check again.";
            cmdBox.classList.remove("hidden");
            primaryBtn.textContent = "Check again";
            currentAction = "retry";
        }
    } catch {
        pillEl.className = "pill bad";
        pillEl.textContent = "Extension worker offline";
        titleEl.textContent = "Extension background service did not answer";
        copyEl.textContent = "Reload the extension in chrome://extensions or click Check again.";
        cmdBox.classList.remove("hidden");
        primaryBtn.textContent = "Check again";
        currentAction = "retry";
    }
}

primaryBtn.addEventListener("click", () => {
    if (currentAction === "reload") {
        chrome.runtime.reload();
        return;
    }
    if (currentAction === "retry") {
        inspect();
        return;
    }
    window.close();
});

retryBtn.addEventListener("click", inspect);
optionsBtn.addEventListener("click", () => chrome.runtime.openOptionsPage());

copyCmdBtn.addEventListener("click", async () => {
    const cmd = document.getElementById("cmd-text").textContent || "ripdl register-host";
    try {
        await navigator.clipboard.writeText(cmd);
        copyCmdLbl.textContent = "Copied";
        setTimeout(() => { copyCmdLbl.textContent = "Copy"; }, 1800);
    } catch {
        copyCmdLbl.textContent = "Select manually";
    }
});

inspect();
