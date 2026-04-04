import { Column, Row, Text, Button } from "@w3cos/std"
import "./styles.css"

export default
<Column className="container">
  <Text className="header">CSS Demo</Text>
  <Text className="subtitle">Styles loaded from external .css file</Text>

  <Column className="card">
    <Text className="card-title">External Stylesheets</Text>
    <Text className="card-body">
      W3C OS now supports importing .css files. Selectors match by className
      and element type. Inline styles override CSS rules.
    </Text>
  </Column>

  <Column className="card">
    <Text className="card-title">SCSS Support</Text>
    <Text className="card-body">
      Import .scss files and they will be compiled via the grass engine
      at build time. Variables, nesting, and mixins all work.
    </Text>
  </Column>

  <Row style={{ gap: 12 }}>
    <Button className="btn-primary">Get Started</Button>
    <Button className="btn-secondary">Learn More</Button>
  </Row>

  <Column className="divider" />

  <Text className="footer">Built with W3C OS • CSS + Native</Text>
</Column>
