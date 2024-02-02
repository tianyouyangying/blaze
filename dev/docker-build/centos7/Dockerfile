FROM openeuler/openeuler:22.03-lts-sp1
#FROM centos:8
# install common tools
RUN yum update -y
#RUN yum install -y centos-release-scl epel-release
RUN yum install -y libzip unzip wget cmake3 openssl-devel

# install gcc-11
#RUN yum install -y *toolset-12*
RUN yum install -y gcc-toolset-12-gcc*
#RUN yum install -y gcc-toolset-12-gcc gcc-toolset-12-gcc-c++
#RUN yum install -y devtoolset-11-gcc devtoolset-11-gcc-c++
#RUN echo '. /opt/rh/devtoolset-11/enable' >> ~/.bashrc
RUN echo '. /opt/rh/gcc-toolset-12/enable' >> ~/.bashrc 
RUN yum install -y gcc
# install rust nightly toolchain
RUN curl https://sh.rustup.rs > /rustup-init
RUN chmod +x /rustup-init
RUN /rustup-init -y --default-toolchain nightly-2023-08-01-x86_64-unknown-linux-gnu

# install java
RUN yum install -y java-1.8.0-openjdk java-1.8.0-openjdk-devel
RUN echo 'export JAVA_HOME="/usr/lib/jvm/java-1.8.0-openjdk"' >> ~/.bashrc

# install maven
#RUN yum install -y maven
#RUN echo 'source /opt/rh/rh-maven35/enable' >> ~/.bashrc
RUN cd /opt/ && wget https://archive.apache.org/dist/maven/maven-3/3.6.1/binaries/apache-maven-3.6.1-bin.tar.gz && tar -xvf apache-maven-3.6.1-bin.tar.gz 
RUN echo 'export PATH="$PATH:/opt/apache-maven-3.6.1/bin"' >> ~/.bashrc
# install protoc
RUN wget -O /protobuf-21.7-linux-x86_64.zip https://github.com/protocolbuffers/protobuf/releases/download/v21.7/protoc-21.7-linux-x86_64.zip
RUN mkdir /protobuf-bin && (cd /protobuf-bin && unzip /protobuf-21.7-linux-x86_64.zip)
RUN echo 'export PATH="$PATH:/protobuf-bin/bin"' >> ~/.bashrc
