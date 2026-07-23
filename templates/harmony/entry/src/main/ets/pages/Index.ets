@Entry
@Component
struct Index {
  build() {
    Stack() {
      XComponent({
        id: 'w3cos-content',
        type: XComponentType.SURFACE,
        libraryName: 'w3cos_harmony_host'
      })
        .width('100%')
        .height('100%')
    }
    .width('100%')
    .height('100%')
    .backgroundColor('#0B1220')
  }
}
