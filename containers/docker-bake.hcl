variable "REGISTRY" { default = "ghcr.io/accelero/agent-platform" }
variable "IMAGE_TAG" { default = "dev" }
variable "SOURCE_REVISION" { default = "" }

function "image" {
  params = [name]
  result = "${REGISTRY}/${name}:${IMAGE_TAG}"
}

group "default" {
  targets = ["hermes", "runtime-manager", "agent-runtime", "multica-backend", "tools", "hermes-manager"]
}
target "metadata" {
  labels = {
    "org.opencontainers.image.source" = "https://github.com/Accelero/agent-platform"
    "org.opencontainers.image.revision" = SOURCE_REVISION
  }
}
target "platform-tools" {
  inherits = ["metadata"]
  context = "tools/platform"
}
target "agent-runtime" {
  inherits = ["metadata"]
  context = "runtimes/agent-runtime"
  tags = [image("agent-runtime")]
}
target "runtime-manager" {
  inherits = ["metadata"]
  context = "services/runtime-manager"
  contexts = { platform-tools = "target:platform-tools" }
  tags = [image("runtime-manager")]
}
target "hermes" {
  inherits = ["metadata"]
  context = "."
  dockerfile = "runtimes/hermes/Dockerfile"
  tags = [image("hermes")]
}
target "multica-backend" {
  inherits = ["metadata"]
  context = "services/multica"
  contexts = { multica-source = "vendor/multica" }
  tags = [image("multica-backend")]
}
target "tools" {
  inherits = ["metadata"]
  context = "."
  dockerfile = "services/tools/Dockerfile"
  contexts = { multica-backend = "target:multica-backend" }
  tags = [image("tools")]
}

target "hermes-manager" {
  inherits = ["metadata"]
  context = "."
  dockerfile = "services/hermes-manager/Dockerfile"
  tags = [image("hermes-manager")]
}
