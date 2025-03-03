FROM python:3.12

RUN apt-get update && apt-get install -y git clang

WORKDIR /usr/src/app
COPY . .
ENV CC=clang
ENV PIP_EXTRA_INDEX_URL=http://bismuth-pypi.s3-website-us-west-2.amazonaws.com/simple
ENV PIP_TRUSTED_HOST=bismuth-pypi.s3-website-us-west-2.amazonaws.com
RUN pip install --no-cache-dir '.[deploy]'

EXPOSE 8000

ARG COMMIT=dev
ENV OTEL_RESOURCE_ATTRIBUTES="service.version=${COMMIT}"

CMD ["uvicorn", "serving:app", "--host", "0.0.0.0", "--port", "8765"]
