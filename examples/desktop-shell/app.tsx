import { Column, Row, Text, Button } from "@w3cos/std"

const activeApp = signal(0)
const showLauncher = signal(0)
const clock = signal(0)

export default
<Column style={{ background: "#0a0a14", gap: 0 }}>
  {/* Desktop area */}
  <Column style={{ flexGrow: 1, padding: 0, gap: 0 }}>

    {/* App window area */}
    <Row style={{ flexGrow: 1, gap: 0 }}>
      {/* Desktop wallpaper with app launcher overlay */}
      <Column style={{
        flexGrow: 1,
        background: "#0f1923",
        padding: 40,
        gap: 24,
        alignItems: "center",
        justifyContent: "center"
      }}>
        {/* Desktop icons */}
        <Row style={{ gap: 32 }}>
          <Column style={{ alignItems: "center", gap: 8 }} onClick="set:activeApp:1">
            <Column style={{
              width: "56", height: "56",
              background: "#1a1a2e",
              borderRadius: 12,
              alignItems: "center",
              justifyContent: "center"
            }}>
              <Text style={{ fontSize: 28 }}>📁</Text>
            </Column>
            <Text style={{ fontSize: 11, color: "#c0c0d0" }}>Files</Text>
          </Column>

          <Column style={{ alignItems: "center", gap: 8 }} onClick="set:activeApp:2">
            <Column style={{
              width: "56", height: "56",
              background: "#1a1a2e",
              borderRadius: 12,
              alignItems: "center",
              justifyContent: "center"
            }}>
              <Text style={{ fontSize: 28 }}>⌨</Text>
            </Column>
            <Text style={{ fontSize: 11, color: "#c0c0d0" }}>Terminal</Text>
          </Column>

          <Column style={{ alignItems: "center", gap: 8 }} onClick="set:activeApp:3">
            <Column style={{
              width: "56", height: "56",
              background: "#1a1a2e",
              borderRadius: 12,
              alignItems: "center",
              justifyContent: "center"
            }}>
              <Text style={{ fontSize: 28 }}>⚙</Text>
            </Column>
            <Text style={{ fontSize: 11, color: "#c0c0d0" }}>Settings</Text>
          </Column>

          <Column style={{ alignItems: "center", gap: 8 }} onClick="set:activeApp:4">
            <Column style={{
              width: "56", height: "56",
              background: "#1a1a2e",
              borderRadius: 12,
              alignItems: "center",
              justifyContent: "center"
            }}>
              <Text style={{ fontSize: 28 }}>🤖</Text>
            </Column>
            <Text style={{ fontSize: 11, color: "#c0c0d0" }}>AI Agent</Text>
          </Column>

          <Column style={{ alignItems: "center", gap: 8 }} onClick="set:activeApp:5">
            <Column style={{
              width: "56", height: "56",
              background: "#1a1a2e",
              borderRadius: 12,
              alignItems: "center",
              justifyContent: "center"
            }}>
              <Text style={{ fontSize: 28 }}>🌐</Text>
            </Column>
            <Text style={{ fontSize: 11, color: "#c0c0d0" }}>Browser</Text>
          </Column>

          <Column style={{ alignItems: "center", gap: 8 }} onClick="set:activeApp:6">
            <Column style={{
              width: "56", height: "56",
              background: "#1a1a2e",
              borderRadius: 12,
              alignItems: "center",
              justifyContent: "center"
            }}>
              <Text style={{ fontSize: 28 }}>📝</Text>
            </Column>
            <Text style={{ fontSize: 11, color: "#c0c0d0" }}>Editor</Text>
          </Column>
        </Row>

        {/* W3C OS branding */}
        <Column style={{ alignItems: "center", gap: 4, opacity: 0.4 }}>
          <Text style={{ fontSize: 16, color: "#6c5ce7", fontWeight: 700 }}>W3C OS</Text>
          <Text style={{ fontSize: 11, color: "#808090" }}>Native Desktop Shell</Text>
        </Column>
      </Column>
    </Row>
  </Column>

  {/* Taskbar */}
  <Row style={{
    height: "48",
    background: "#12121f",
    padding: 8,
    gap: 8,
    alignItems: "center",
    justifyContent: "spaceBetween"
  }}>
    {/* Left: App launcher + pinned apps */}
    <Row style={{ gap: 6, alignItems: "center" }}>
      <Button style={{
        background: "#6c5ce7",
        borderRadius: 8,
        fontSize: 14,
        color: "#ffffff"
      }} onClick="toggle:showLauncher">◆ Apps</Button>

      {/* Pinned/running app indicators */}
      <Column style={{
        width: "36", height: "36",
        background: "#1c1c34",
        borderRadius: 8,
        alignItems: "center",
        justifyContent: "center"
      }} onClick="set:activeApp:1">
        <Text style={{ fontSize: 18 }}>📁</Text>
      </Column>
      <Column style={{
        width: "36", height: "36",
        background: "#1c1c34",
        borderRadius: 8,
        alignItems: "center",
        justifyContent: "center"
      }} onClick="set:activeApp:2">
        <Text style={{ fontSize: 18 }}>⌨</Text>
      </Column>
      <Column style={{
        width: "36", height: "36",
        background: "#1c1c34",
        borderRadius: 8,
        alignItems: "center",
        justifyContent: "center"
      }} onClick="set:activeApp:3">
        <Text style={{ fontSize: 18 }}>⚙</Text>
      </Column>
    </Row>

    {/* Center: Window title */}
    <Text style={{ fontSize: 13, color: "#808090" }}>W3C OS Desktop</Text>

    {/* Right: System tray */}
    <Row style={{ gap: 12, alignItems: "center" }}>
      <Text style={{ fontSize: 12, color: "#00b894" }}>● Online</Text>
      <Text style={{ fontSize: 12, color: "#a0a0c0" }}>🔋 92%</Text>
      <Text style={{ fontSize: 12, color: "#a0a0c0" }}>🔊</Text>
      <Column style={{
        background: "#1c1c34",
        borderRadius: 6,
        padding: 6
      }}>
        <Text style={{ fontSize: 12, color: "#d0d0e0" }}>14:32</Text>
      </Column>
    </Row>
  </Row>
</Column>
