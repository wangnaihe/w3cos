import { Column, Row, Text, Button } from "@w3cos/std"
import "./theme.scss"

export default
<Column className="container">
  <Text className="title">SCSS Demo</Text>

  <Column className="card">
    <Row style={{ justifyContent: "space-between", alignItems: "center" }}>
      <Text className="card-heading">Variables & Mixins</Text>
      <Text className="badge">SCSS</Text>
    </Row>
    <Text className="card-text">
      This example uses SCSS variables ($accent, $bg-dark) and mixins
      (@mixin card-base). Compiled at build time via the grass engine.
    </Text>
  </Column>

  <Column className="card">
    <Text className="card-heading">Zero Runtime Cost</Text>
    <Text className="card-text">
      SCSS is compiled to CSS, then resolved at compile time. The final
      binary has no CSS parser — just direct Style structs in Rust.
    </Text>
  </Column>

  <Button className="btn-accent">Explore W3C OS</Button>

  <Text className="muted">Theme powered by SCSS variables</Text>
</Column>
