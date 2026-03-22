import { Column, Row, Text } from "@w3cos/std"

export default
<Column style={{ gap: 12, padding: 32, background: "#0f0f1a", width: 380, borderRadius: 16 }}>
  <Text style={{ fontSize: 28, color: "#ffffff", fontWeight: 700, marginBottom: 8 }}>Weather</Text>
  <Text style={{ fontSize: 18, color: "#a0a0b0" }}>San Francisco</Text>

  <Column style={{ gap: 8, padding: 24, background: "#1a1a2e", borderRadius: 16, alignItems: "center", marginTop: 16 }}>
    <Text style={{ fontSize: 64 }}>☀️</Text>
    <Text style={{ fontSize: 48, color: "#ffffff", fontWeight: 700 }}>72°F</Text>
    <Text style={{ fontSize: 18, color: "#e94560" }}>Sunny</Text>
  </Column>

  <Row style={{ justifyContent: "space-around", marginTop: 16, padding: 16, background: "#1a1a2e", borderRadius: 12 }}>
    <Column style={{ gap: 4, alignItems: "center" }}>
      <Text style={{ fontSize: 20 }}>💧</Text>
      <Text style={{ fontSize: 14, color: "#ffffff" }}>62%</Text>
      <Text style={{ fontSize: 11, color: "#888899" }}>Humidity</Text>
    </Column>
    <Column style={{ gap: 4, alignItems: "center" }}>
      <Text style={{ fontSize: 20 }}>🌬</Text>
      <Text style={{ fontSize: 14, color: "#ffffff" }}>12 mph</Text>
      <Text style={{ fontSize: 11, color: "#888899" }}>Wind</Text>
    </Column>
    <Column style={{ gap: 4, alignItems: "center" }}>
      <Text style={{ fontSize: 20 }}>👁</Text>
      <Text style={{ fontSize: 14, color: "#ffffff" }}>10 mi</Text>
      <Text style={{ fontSize: 11, color: "#888899" }}>Visibility</Text>
    </Column>
  </Row>

  <Text style={{ fontSize: 16, color: "#e94560", fontWeight: 600, marginTop: 16, marginBottom: 4 }}>5-Day Forecast</Text>
  <Row style={{ justifyContent: "space-around", padding: 12, background: "#1a1a2e", borderRadius: 12 }}>
    <Column style={{ gap: 4, alignItems: "center" }}>
      <Text style={{ fontSize: 12, color: "#888899" }}>Mon</Text>
      <Text style={{ fontSize: 24 }}>☀️</Text>
      <Text style={{ fontSize: 14, color: "#ffffff" }}>75°</Text>
    </Column>
    <Column style={{ gap: 4, alignItems: "center" }}>
      <Text style={{ fontSize: 12, color: "#888899" }}>Tue</Text>
      <Text style={{ fontSize: 24 }}>⛅</Text>
      <Text style={{ fontSize: 14, color: "#ffffff" }}>68°</Text>
    </Column>
    <Column style={{ gap: 4, alignItems: "center" }}>
      <Text style={{ fontSize: 12, color: "#888899" }}>Wed</Text>
      <Text style={{ fontSize: 24 }}>🌧</Text>
      <Text style={{ fontSize: 14, color: "#ffffff" }}>61°</Text>
    </Column>
    <Column style={{ gap: 4, alignItems: "center" }}>
      <Text style={{ fontSize: 12, color: "#888899" }}>Thu</Text>
      <Text style={{ fontSize: 24 }}>🌧</Text>
      <Text style={{ fontSize: 14, color: "#ffffff" }}>59°</Text>
    </Column>
    <Column style={{ gap: 4, alignItems: "center" }}>
      <Text style={{ fontSize: 12, color: "#888899" }}>Fri</Text>
      <Text style={{ fontSize: 24 }}>☀️</Text>
      <Text style={{ fontSize: 14, color: "#ffffff" }}>73°</Text>
    </Column>
  </Row>
</Column>
