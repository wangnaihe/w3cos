import { Column, Row, Text, Button } from "@w3cos/std"

export default
<Column style={{ gap: 16, padding: 32, alignItems: "center", background: "#1a1a2e", borderRadius: 16, width: 320 }}>
  <Text style={{ fontSize: 24, color: "#e94560", fontWeight: 700, marginBottom: 8 }}>Calculator</Text>
  <Text style={{ fontSize: 48, color: "#ffffff", fontWeight: 700, padding: 16, background: "#16213e", borderRadius: 12, width: "100%", textAlign: "right", marginBottom: 8 }}>42</Text>
  <Row style={{ gap: 8, width: "100%" }}>
    <Button style={{ flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 }}>7</Button>
    <Button style={{ flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 }}>8</Button>
    <Button style={{ flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 }}>9</Button>
    <Button style={{ flex: 1, padding: 16, background: "#e94560", color: "#fff", borderRadius: 8, fontSize: 20 }}>/</Button>
  </Row>
  <Row style={{ gap: 8, width: "100%" }}>
    <Button style={{ flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 }}>4</Button>
    <Button style={{ flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 }}>5</Button>
    <Button style={{ flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 }}>6</Button>
    <Button style={{ flex: 1, padding: 16, background: "#e94560", color: "#fff", borderRadius: 8, fontSize: 20 }}>*</Button>
  </Row>
  <Row style={{ gap: 8, width: "100%" }}>
    <Button style={{ flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 }}>1</Button>
    <Button style={{ flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 }}>2</Button>
    <Button style={{ flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 }}>3</Button>
    <Button style={{ flex: 1, padding: 16, background: "#e94560", color: "#fff", borderRadius: 8, fontSize: 20 }}>-</Button>
  </Row>
  <Row style={{ gap: 8, width: "100%" }}>
    <Button style={{ flex: 2, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 }}>0</Button>
    <Button style={{ flex: 1, padding: 16, background: "#0f3460", color: "#fff", borderRadius: 8, fontSize: 20 }}>.</Button>
    <Button style={{ flex: 1, padding: 16, background: "#e94560", color: "#fff", borderRadius: 8, fontSize: 20 }}>=</Button>
  </Row>
</Column>
