// FerriumWM launcher (Quickshell).
//   qs -c fewm-launcher                               — run (FeWM autostart)
//   qs ipc -c fewm-launcher call launcher toggle      — show/hide (Mod+D)
// Colors follow the FeWM wallpaper palette (~/.cache/FerriumWM/colors.json).
// Search apps; "=2+2" calculator (Enter copies); ">cmd" runs a shell command.

import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import Quickshell.Widgets

ShellRoot {
    id: root

    property bool open: false
    property string query: ""
    property int selected: 0
    property var results: []
    property var usage: ({})
    property string terminal: Quickshell.env("TERMINAL") || "kitty"

    property var pal: ({
        "surface": "#131314",
        "surface_container": "#1f1f22",
        "surface_container_high": "#2a2a2e",
        "on_surface": "#e5e2e2",
        "on_surface_variant": "#c4c6cf",
        "primary": "#a8c8ff",
        "on_primary": "#00315f",
        "primary_container": "#2c4a75",
        "on_primary_container": "#d5e3ff",
        "outline": "#8e9099",
        "outline_variant": "#44474e",
        "error": "#ffb4ab"
    })
    function c(name) { return pal[name] || "#ff00ff" }

    FileView {
        id: colorsFile
        path: Quickshell.env("HOME") + "/.cache/FerriumWM/colors.json"
        watchChanges: true
        onFileChanged: reload()
        onLoaded: {
            try {
                root.pal = Object.assign({}, root.pal, JSON.parse(text()))
            } catch (e) {}
        }
    }

    FileView {
        id: usageFile
        path: Quickshell.env("HOME") + "/.cache/FerriumWM/launcher-usage.json"
        onLoaded: {
            try {
                root.usage = JSON.parse(text())
            } catch (e) {
                root.usage = {}
            }
        }
    }

    IpcHandler {
        target: "launcher"
        function toggle(): void { root.setOpen(!root.open) }
        function show(): void { root.setOpen(true) }
        function hide(): void { root.setOpen(false) }
    }

    function setOpen(v) {
        if (v) {
            query = ""
            selected = 0
            results = compute()
        }
        open = v
    }

    onQueryChanged: {
        results = compute()
        selected = 0
    }

    // ---------------------------------------------------------------- search

    function score(hay, q) {
        if (!hay || !q) return 0
        hay = String(hay).toLowerCase()
        const i = hay.indexOf(q)
        if (i === 0) return 100
        if (i > 0) {
            // word start beats middle of a word
            const wordStart = /[\s\-_.]/.test(hay[i - 1])
            return (wordStart ? 85 : 70) - Math.min(i, 30) * 0.5
        }
        // ordered subsequence ("ffx" → firefox)
        let pos = 0, gaps = 0
        for (const ch of q) {
            const f = hay.indexOf(ch, pos)
            if (f < 0) return 0
            gaps += f - pos
            pos = f + 1
        }
        return Math.max(1, 40 - gaps * 2)
    }

    function compute() {
        const raw = query.trim()
        const q = raw.toLowerCase()

        if (raw.startsWith("=")) {
            const expr = raw.slice(1).replace(/,/g, ".").replace(/\^/g, "**").replace(/×/g, "*").replace(/÷/g, "/")
            let value = ""
            if (/^[0-9+\-*/().%\s*]+$/.test(expr) && expr.trim() !== "") {
                try {
                    const r = Function('"use strict"; return (' + expr + ')')()
                    if (typeof r === "number" && isFinite(r)) value = String(Math.round(r * 1e10) / 1e10)
                } catch (e) {}
            }
            return [{ kind: "calc", name: value === "" ? "…" : "= " + value, desc: value === "" ? "калькулятор" : "Enter — скопировать", icon: "accessories-calculator", value: value }]
        }
        if (raw.startsWith(">")) {
            const cmd = raw.slice(1).trim()
            return [{ kind: "run", name: cmd === "" ? "Команда…" : cmd, desc: "Enter — выполнить в shell", icon: "utilities-terminal", cmd: cmd }]
        }

        const apps = DesktopEntries.applications.values
        const out = []
        for (let k = 0; k < apps.length; k++) {
            const e = apps[k]
            if (e.noDisplay) continue
            let s = 1
            if (q !== "") {
                s = Math.max(
                    score(e.name, q),
                    score(e.genericName, q) * 0.7,
                    score((e.keywords || []).join(" "), q) * 0.6,
                    score(e.id, q) * 0.5,
                    score((e.command || []).join(" "), q) * 0.3
                )
                if (s <= 0) continue
            }
            s += Math.min(usage[e.id] || 0, 40) * (q === "" ? 3 : 0.6)
            out.push({ kind: "app", entry: e, id: e.id, name: e.name, desc: e.genericName || e.comment || "", icon: e.icon, score: s })
        }
        out.sort((a, b) => (b.score - a.score) || a.name.localeCompare(b.name))
        return out.slice(0, 80)
    }

    function activate(i) {
        const r = results[i]
        if (!r) return
        if (r.kind === "app") {
            const u = Object.assign({}, usage)
            u[r.id] = (u[r.id] || 0) + 1
            usage = u
            usageFile.setText(JSON.stringify(u))
            if (r.entry.runInTerminal)
                Quickshell.execDetached([terminal, "-e"].concat(r.entry.command))
            else
                r.entry.execute()
        } else if (r.kind === "calc") {
            if (r.value !== "") Quickshell.execDetached(["wl-copy", r.value])
        } else if (r.kind === "run") {
            if (r.cmd !== "") Quickshell.execDetached(["sh", "-c", r.cmd])
        }
        setOpen(false)
    }

    // ---------------------------------------------------------------- UI

    PanelWindow {
        id: win
        visible: root.open
        color: "transparent"
        anchors {
            top: true
            bottom: true
            left: true
            right: true
        }
        exclusionMode: ExclusionMode.Ignore
        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.namespace: "fewm-launcher"
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive

        onVisibleChanged: {
            if (visible) {
                box.opacity = 0
                box.scale = 0.97
                appear.restart()
                input.text = ""
                input.forceActiveFocus()
            }
        }

        // dim + click outside closes
        Rectangle {
            anchors.fill: parent
            color: "#000000"
            opacity: 0.35 * box.opacity
        }
        MouseArea {
            anchors.fill: parent
            onClicked: root.setOpen(false)
        }

        ParallelAnimation {
            id: appear
            NumberAnimation { target: box; property: "opacity"; to: 1; duration: 120; easing.type: Easing.OutCubic }
            NumberAnimation { target: box; property: "scale"; to: 1; duration: 140; easing.type: Easing.OutCubic }
        }

        Rectangle {
            id: box
            width: Math.min(680, win.width - 80)
            height: Math.min(540, win.height - 140)
            anchors.centerIn: parent
            color: root.c("surface_container")
            border.color: root.c("outline_variant")
            border.width: 2
            radius: 0

            MouseArea { anchors.fill: parent } // swallow clicks inside

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 14
                spacing: 10

                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 48
                    color: root.c("surface_container_high")
                    border.color: root.c("primary")
                    border.width: 2

                    Text {
                        id: prompt
                        anchors.left: parent.left
                        anchors.leftMargin: 14
                        anchors.verticalCenter: parent.verticalCenter
                        text: query.startsWith("=") ? "=" : query.startsWith(">") ? ">" : "›"
                        color: root.c("primary")
                        font.pixelSize: 22
                        font.bold: true
                    }

                    TextInput {
                        id: input
                        anchors.left: prompt.right
                        anchors.leftMargin: 10
                        anchors.right: parent.right
                        anchors.rightMargin: 14
                        anchors.verticalCenter: parent.verticalCenter
                        color: root.c("on_surface")
                        selectionColor: root.c("primary")
                        selectedTextColor: root.c("on_primary")
                        font.pixelSize: 18
                        focus: true
                        clip: true
                        onTextChanged: root.query = text

                        Keys.onPressed: (ev) => {
                            const n = root.results.length
                            if (ev.key === Qt.Key_Escape) {
                                root.setOpen(false)
                            } else if (ev.key === Qt.Key_Down || (ev.key === Qt.Key_Tab && !(ev.modifiers & Qt.ShiftModifier)) || (ev.key === Qt.Key_J && (ev.modifiers & Qt.ControlModifier)) || (ev.key === Qt.Key_N && (ev.modifiers & Qt.ControlModifier))) {
                                if (n > 0) root.selected = (root.selected + 1) % n
                            } else if (ev.key === Qt.Key_Up || ev.key === Qt.Key_Backtab || (ev.key === Qt.Key_K && (ev.modifiers & Qt.ControlModifier)) || (ev.key === Qt.Key_P && (ev.modifiers & Qt.ControlModifier))) {
                                if (n > 0) root.selected = (root.selected - 1 + n) % n
                            } else if (ev.key === Qt.Key_PageDown) {
                                root.selected = Math.min(n - 1, root.selected + 8)
                            } else if (ev.key === Qt.Key_PageUp) {
                                root.selected = Math.max(0, root.selected - 8)
                            } else if (ev.key === Qt.Key_Return || ev.key === Qt.Key_Enter) {
                                root.activate(root.selected)
                            } else {
                                return
                            }
                            ev.accepted = true
                        }
                    }

                    Text {
                        anchors.left: input.left
                        anchors.verticalCenter: parent.verticalCenter
                        visible: input.text === ""
                        text: "Поиск приложений…   =2+2 калькулятор   >команда"
                        color: root.c("on_surface_variant")
                        opacity: 0.6
                        font.pixelSize: 16
                    }
                }

                ListView {
                    id: list
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true
                    model: root.results
                    currentIndex: root.selected
                    boundsBehavior: Flickable.StopAtBounds
                    highlightMoveDuration: 0
                    onCurrentIndexChanged: positionViewAtIndex(currentIndex, ListView.Contain)

                    delegate: Rectangle {
                        required property var modelData
                        required property int index
                        width: list.width
                        height: 50
                        color: index === root.selected ? root.c("primary_container") : "transparent"

                        RowLayout {
                            anchors.fill: parent
                            anchors.leftMargin: 12
                            anchors.rightMargin: 12
                            spacing: 12

                            IconImage {
                                Layout.preferredWidth: 30
                                Layout.preferredHeight: 30
                                implicitSize: 30
                                source: modelData.icon ? Quickshell.iconPath(modelData.icon, "application-x-executable") : ""
                            }

                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 1
                                Text {
                                    Layout.fillWidth: true
                                    text: modelData.name
                                    elide: Text.ElideRight
                                    font.pixelSize: 15
                                    font.bold: true
                                    color: index === root.selected ? root.c("on_primary_container") : root.c("on_surface")
                                }
                                Text {
                                    Layout.fillWidth: true
                                    visible: text !== ""
                                    text: modelData.desc
                                    elide: Text.ElideRight
                                    font.pixelSize: 12
                                    color: index === root.selected ? root.c("on_primary_container") : root.c("on_surface_variant")
                                    opacity: 0.85
                                }
                            }
                        }

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            onEntered: root.selected = index
                            onClicked: root.activate(index)
                        }
                    }
                }

                Text {
                    Layout.fillWidth: true
                    text: root.results.length + " · ↑↓ выбор · Enter запуск · Esc закрыть"
                    color: root.c("on_surface_variant")
                    opacity: 0.6
                    font.pixelSize: 11
                }
            }
        }
    }
}
