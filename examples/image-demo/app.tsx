import { Column, Row, Text, Image } from "@w3cos/std"

export default
<Column style={{ gap: 24, padding: 48, alignItems: "center", background: "#0f0f1a" }}>
  <Text style={{ fontSize: 32, color: "#e94560", fontWeight: 700 }}>Image Component Demo</Text>
  <Text style={{ fontSize: 16, color: "#a0a0b0" }}>PNG and JPEG images rendered natively via tiny-skia / Vello</Text>

  <Row style={{ gap: 24, padding: 24, alignItems: "center" }}>
    <Column style={{ gap: 8, alignItems: "center" }}>
      <Image src="https://picsum.photos/seed/w3cos/300/200" style={{ width: "300px", height: "200px", borderRadius: 12 }} />
      <Text style={{ fontSize: 14, color: "#606070" }}>Remote JPEG (300×200)</Text>
    </Column>

    <Column style={{ gap: 8, alignItems: "center" }}>
      <Image src="https://www.rust-lang.org/logos/rust-logo-512x512.png" style={{ width: "200px", height: "200px", borderRadius: 8 }} />
      <Text style={{ fontSize: 14, color: "#606070" }}>Remote PNG (200×200)</Text>
    </Column>
  </Row>

  <Text style={{ fontSize: 14, color: "#404050" }}>Supports local file paths and http/https URLs</Text>
</Column>
