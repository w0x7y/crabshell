// Like-for-like Quickshell baseline for the Crabshell Phase 0 spike.
// The same static bar as spike/: one top-anchored panel on eDP-1, 30 px tall,
// exclusive zone 30, background #16181d, "Crabshell" in sans-serif 15 px #e6e8ee
// with 8 px left padding. No services, no animations.
//
// Run as a separate instance by path (doesn't touch other running configs):
//   qs -p baseline/quickshell

import QtQuick
import Quickshell

PanelWindow {
    screen: {
        const screens = Quickshell.screens;
        for (let i = 0; i < screens.length; i++) {
            if (screens[i].name === "eDP-1")
                return screens[i];
        }
        return null;
    }

    // Layer defaults to Top and keyboard focus to None, as in the spike.
    anchors {
        top: true
        left: true
        right: true
    }
    implicitHeight: 30
    exclusiveZone: 30
    color: "#16181d"

    Text {
        anchors.left: parent.left
        anchors.leftMargin: 8
        anchors.verticalCenter: parent.verticalCenter
        text: "Crabshell"
        color: "#e6e8ee"
        font.family: "sans-serif"
        font.pixelSize: 15
    }
}
