import { Column, Row, Text, Button } from "@w3cos/std"

// Adaptive Layout Demo — one codebase, works on all screen sizes.
// Uses CSS Flexbox + viewport units + min/max constraints.
// No @media queries needed for basic responsiveness — flex-wrap handles it.
//
// This layout automatically adapts:
//   Desktop (>1024px): sidebar + 3-column grid
//   Tablet  (600-1024): sidebar + 2-column grid
//   Phone   (<600px): stacked, sidebar becomes top nav

const activeTab = signal(0)

export default
<Column style={{ background: "#0a0a14", gap: 0 }}>
  {/* Top bar — always visible */}
  <Row style={{
    padding: 12,
    background: "#12121f",
    alignItems: "center",
    justifyContent: "spaceBetween"
  }}>
    <Row style={{ gap: 8, alignItems: "center" }}>
      <Text style={{ fontSize: 18, color: "#6c5ce7" }}>◆</Text>
      <Text style={{ fontSize: 16, color: "#e0e0f0", fontWeight: 700 }}>Adaptive App</Text>
    </Row>
    <Text style={{ fontSize: 12, color: "#808090" }}>One codebase, any screen</Text>
  </Row>

  {/* Main content — flex-wrap makes it responsive */}
  <Row style={{
    flexGrow: 1,
    gap: 0,
    flexWrap: "wrap",
    alignItems: "flexStart"
  }}>
    {/* Sidebar: 220px on desktop, full width on phone (wraps below minWidth) */}
    <Column style={{
      width: "20vw",
      minWidth: "200",
      maxWidth: "280",
      padding: 16,
      gap: 4,
      background: "#10101c"
    }}>
      <Text style={{ fontSize: 11, color: "#505070", fontWeight: 700 }}>NAVIGATION</Text>
      <Row style={{ padding: 10, borderRadius: 8, background: "#6c5ce7", gap: 8, alignItems: "center" }} onClick="set:activeTab:0">
        <Text style={{ fontSize: 14, color: "#ffffff" }}>⬡ Dashboard</Text>
      </Row>
      <Row style={{ padding: 10, borderRadius: 8, gap: 8, alignItems: "center" }} onClick="set:activeTab:1">
        <Text style={{ fontSize: 14, color: "#a0a0c0" }}>◈ Analytics</Text>
      </Row>
      <Row style={{ padding: 10, borderRadius: 8, gap: 8, alignItems: "center" }} onClick="set:activeTab:2">
        <Text style={{ fontSize: 14, color: "#a0a0c0" }}>◉ Users</Text>
      </Row>
      <Row style={{ padding: 10, borderRadius: 8, gap: 8, alignItems: "center" }} onClick="set:activeTab:3">
        <Text style={{ fontSize: 14, color: "#a0a0c0" }}>⚙ Settings</Text>
      </Row>
    </Column>

    {/* Content area: fills remaining space */}
    <Column style={{
      flexGrow: 1,
      minWidth: "300",
      padding: 20,
      gap: 16
    }}>
      <Text style={{ fontSize: 22, color: "#f0f0ff", fontWeight: 700 }}>Dashboard</Text>

      {/* Stats cards — flex-wrap for responsive grid */}
      <Row style={{ gap: 12, flexWrap: "wrap" }}>
        <Column style={{
          flexGrow: 1,
          minWidth: "200",
          padding: 16,
          background: "#16162a",
          borderRadius: 12,
          gap: 6
        }}>
          <Text style={{ fontSize: 12, color: "#808090" }}>Total Users</Text>
          <Text style={{ fontSize: 28, color: "#f0f0ff", fontWeight: 700 }}>12,847</Text>
          <Text style={{ fontSize: 12, color: "#00b894" }}>↑ 12.5%</Text>
        </Column>

        <Column style={{
          flexGrow: 1,
          minWidth: "200",
          padding: 16,
          background: "#16162a",
          borderRadius: 12,
          gap: 6
        }}>
          <Text style={{ fontSize: 12, color: "#808090" }}>Revenue</Text>
          <Text style={{ fontSize: 28, color: "#f0f0ff", fontWeight: 700 }}>$84.2K</Text>
          <Text style={{ fontSize: 12, color: "#00b894" }}>↑ 8.3%</Text>
        </Column>

        <Column style={{
          flexGrow: 1,
          minWidth: "200",
          padding: 16,
          background: "#16162a",
          borderRadius: 12,
          gap: 6
        }}>
          <Text style={{ fontSize: 12, color: "#808090" }}>Active Now</Text>
          <Text style={{ fontSize: 28, color: "#f0f0ff", fontWeight: 700 }}>1,429</Text>
          <Text style={{ fontSize: 12, color: "#fdcb6e" }}>● Live</Text>
        </Column>

        <Column style={{
          flexGrow: 1,
          minWidth: "200",
          padding: 16,
          background: "#16162a",
          borderRadius: 12,
          gap: 6
        }}>
          <Text style={{ fontSize: 12, color: "#808090" }}>Conversion</Text>
          <Text style={{ fontSize: 28, color: "#f0f0ff", fontWeight: 700 }}>3.2%</Text>
          <Text style={{ fontSize: 12, color: "#e94560" }}>↓ 0.4%</Text>
        </Column>
      </Row>

      {/* Two-column section: wraps to single on narrow */}
      <Row style={{ gap: 16, flexWrap: "wrap" }}>
        <Column style={{
          flexGrow: 2,
          minWidth: "300",
          padding: 16,
          background: "#16162a",
          borderRadius: 12,
          gap: 8
        }}>
          <Text style={{ fontSize: 16, color: "#f0f0ff", fontWeight: 700 }}>Recent Activity</Text>
          <Row style={{ padding: 10, background: "#1c1c34", borderRadius: 6, justifyContent: "spaceBetween" }}>
            <Text style={{ fontSize: 13, color: "#d0d0e0" }}>New signup: alice@example.com</Text>
            <Text style={{ fontSize: 11, color: "#808090" }}>2m ago</Text>
          </Row>
          <Row style={{ padding: 10, background: "#1c1c34", borderRadius: 6, justifyContent: "spaceBetween" }}>
            <Text style={{ fontSize: 13, color: "#d0d0e0" }}>Purchase: Pro Plan</Text>
            <Text style={{ fontSize: 11, color: "#808090" }}>5m ago</Text>
          </Row>
          <Row style={{ padding: 10, background: "#1c1c34", borderRadius: 6, justifyContent: "spaceBetween" }}>
            <Text style={{ fontSize: 13, color: "#d0d0e0" }}>Feedback: "Great product!"</Text>
            <Text style={{ fontSize: 11, color: "#808090" }}>12m ago</Text>
          </Row>
        </Column>

        <Column style={{
          flexGrow: 1,
          minWidth: "200",
          padding: 16,
          background: "#16162a",
          borderRadius: 12,
          gap: 8
        }}>
          <Text style={{ fontSize: 16, color: "#f0f0ff", fontWeight: 700 }}>Quick Actions</Text>
          <Button style={{ background: "#6c5ce7", borderRadius: 8, fontSize: 13, color: "#ffffff" }}>New Campaign</Button>
          <Button style={{ background: "#1c1c34", borderRadius: 8, fontSize: 13, color: "#a0a0c0" }}>Export Data</Button>
          <Button style={{ background: "#1c1c34", borderRadius: 8, fontSize: 13, color: "#a0a0c0" }}>Invite Team</Button>
        </Column>
      </Row>
    </Column>
  </Row>
</Column>
