// Build-only targets for the project-keyed session image layers.
// Publishing is intentionally owned by estate CI, not this repository file.
variable "TAG" { default = "local" }
variable "REGISTRY" { default = "repo.indexarr.net/indexarr" }

group "default" { targets = ["base", "rust", "mobile"] }

target "common" {
  context = "engine"
  dockerfile = "Dockerfile.pod"
}

target "base" {
  inherits = ["common"]
  target = "base"
  tags = ["${REGISTRY}/vogt-session-base:${TAG}"]
}

target "rust" {
  inherits = ["common"]
  target = "rust"
  tags = ["${REGISTRY}/vogt-session-rust:${TAG}"]
}

target "mobile" {
  inherits = ["common"]
  target = "mobile"
  tags = ["${REGISTRY}/vogt-session-mobile:${TAG}"]
}
