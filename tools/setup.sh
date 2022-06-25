#!/bin/sh

set -e

llvm_version=14

brew install llvm@$llvm_version
llvm_prefix=$(brew --prefix)/opt/llvm@$llvm_version

echo LLVM_SYS_${llvm_version}0_PREFIX=$llvm_prefix >>$GITHUB_ENV
echo PATH=$llvm_prefix/bin:$PATH >>$GITHUB_ENV
