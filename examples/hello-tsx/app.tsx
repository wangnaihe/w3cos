import { Column, Row, Text, Button } from "@w3cos/std"

export default
<Column style={{ gap: 20, padding: 48, alignItems: "center", justifyContent: "center", background: "#0f0f1a" }}>
  <Text style={{ fontSize: 42, color: "#e94560", fontWeight: 700 }}>W3C OS</Text>
  <Text style={{ fontSize: 20, color: "#a0a0b0" }}>Now with TSX syntax support.</Text>
  <Text style={{ fontSize: 16, color: "#606070" }}>No browser. No V8. No Electron. Pure native.</Text>
  <Row style={{ gap: 16, padding: 24 }}>
    <Button style={{ background: "#e94560", color: "#ffffff", borderRadius: 8, fontSize: 16 }}>Get Started</Button>
    <Button style={{ background: "#1a1a2e", color: "#e94560", borderRadius: 8, borderWidth: 1, borderColor: "#e94560", fontSize: 16 }}>View Source</Button>
  </Row>
  <Text style={{ fontSize: 14, color: "#404050" }}>Write JSX. Compile to native. Ship fast.</Text>
</Column>
